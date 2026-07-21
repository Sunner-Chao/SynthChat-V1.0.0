use std::{collections::HashSet, path::Path, sync::Arc};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::profiles::Versioned;

use super::{
    CommitMessage, CreateSession, ListMessages, ListSessions, Message, MessagePage, MessagePart,
    MessageRole, PatchField, RuntimeLease, RuntimeLeaseState, SearchField, SearchMatch, SearchMode,
    SearchRange, Session, SessionError, SessionPage, SessionPatch, StorageReady, StorageState,
    ToolCall, Usage,
    cursor::{CursorCodec, CursorPayload},
    schema,
};

const DEFAULT_TITLE: &str = "New conversation";
const SESSION_SOURCE: &str = "synthchat";
const MAX_TITLE_CHARS: usize = 500;
const MAX_QUERY_CHARS: usize = 500;
const MAX_SESSION_ID_BYTES: usize = 128;
const MAX_MESSAGE_TEXT_CHARS: usize = 1_000_000;
const MAX_PREVIEW_CHARS: usize = 500;

#[derive(Clone)]
pub struct SessionService {
    storage: StorageState,
    cursors: Arc<CursorCodec>,
    runtime_lease: Arc<std::sync::Mutex<RuntimeLeaseState>>,
}

#[derive(Clone, Debug)]
pub(super) struct StoredSession {
    pub(super) value: Session,
    pub(super) next_message_sequence: u64,
}

impl SessionService {
    pub fn new(hermes_home: &Path, desktop_token: &str) -> Self {
        let storage = match schema::initialize(hermes_home) {
            Ok((db_path, search_mode)) => StorageState::Ready(StorageReady {
                db_path: Arc::new(db_path),
                search_mode,
                write_lock: Arc::new(std::sync::Mutex::new(())),
            }),
            Err(_) => StorageState::Unavailable,
        };
        Self {
            storage,
            cursors: Arc::new(CursorCodec::new(desktop_token)),
            runtime_lease: Arc::new(std::sync::Mutex::new(RuntimeLeaseState::Unmanaged)),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self.storage, StorageState::Ready(_))
    }

    pub fn schema_version(&self) -> Option<u32> {
        self.is_available().then_some(super::SESSION_SCHEMA_VERSION)
    }

    pub fn search_mode(&self) -> SearchMode {
        match &self.storage {
            StorageState::Ready(ready) => ready.search_mode,
            StorageState::Unavailable => SearchMode::Unavailable,
        }
    }

    pub(super) fn runtime_lease_state(&self) -> RuntimeLeaseState {
        self.runtime_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn begin_runtime_lease_acquisition(&self) {
        let mut state = self
            .runtime_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, RuntimeLeaseState::Unmanaged) {
            *state = RuntimeLeaseState::Fenced;
        }
    }

    pub(super) fn hold_runtime_lease(&self, lease: RuntimeLease) {
        *self
            .runtime_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = RuntimeLeaseState::Held(lease);
    }

    pub(super) fn fence_runtime_lease(&self, expected: &RuntimeLease) {
        let mut state = self
            .runtime_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(&*state, RuntimeLeaseState::Held(current) if current == expected) {
            *state = RuntimeLeaseState::Fenced;
        }
    }

    pub fn create_session(
        &self,
        request: &CreateSession,
        idempotency_key: &str,
    ) -> Result<Versioned<Session>, SessionError> {
        validate_profile_id(&request.profile_id)?;
        validate_optional_persona_id(request.persona_id.as_deref())?;
        validate_idempotency_key(idempotency_key)?;
        let title = normalize_optional_title(request.title.as_deref())?;
        let fingerprint =
            create_fingerprint(&request.profile_id, &title, request.persona_id.as_deref());
        let ready = self.ready()?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| SessionError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)?;

        if let Some((stored_fingerprint, resource_id)) = transaction
            .query_row(
                "SELECT request_fingerprint, resource_id FROM idempotency_records \
                 WHERE method = 'POST' AND canonical_path = '/api/v1/sessions' \
                   AND idempotency_key = ?1",
                [idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(schema::map_sqlite)?
        {
            if stored_fingerprint != fingerprint {
                return Err(SessionError::IdempotencyConflict);
            }
            let stored = current_session_tx(&transaction, &resource_id)?
                .ok_or(SessionError::IdempotentResourceDeleted)?;
            transaction.commit().map_err(schema::map_sqlite)?;
            return Ok(versioned(stored.value));
        }

        let change = next_change(&transaction)?;
        let id = format!("session_{}", Uuid::new_v4().simple());
        let revision = new_revision();
        let timestamp = now_timestamp()?;
        transaction
            .execute(
                "INSERT INTO sessions(\
                    id, profile_id, title, preview, source, model, message_count, archived, \
                    revision, created_at, updated_at, next_message_sequence, current_change, persona_id\
                 ) VALUES(?1, ?2, ?3, '', ?4, '', 0, 0, ?5, ?6, ?6, 1, ?7, ?8)",
                params![
                    id,
                    request.profile_id,
                    title,
                    SESSION_SOURCE,
                    revision,
                    timestamp,
                    change,
                    request.persona_id,
                ],
            )
            .map_err(schema::map_sqlite)?;
        insert_current_version(&transaction, &id, change)?;
        transaction
            .execute(
                "INSERT INTO idempotency_records(\
                    method, canonical_path, idempotency_key, request_fingerprint, resource_id, created_at\
                 ) VALUES('POST', '/api/v1/sessions', ?1, ?2, ?3, ?4)",
                params![idempotency_key, fingerprint, id, timestamp],
            )
            .map_err(schema::map_sqlite)?;
        let stored = current_session_tx(&transaction, &id)?.ok_or(SessionError::DataInvalid)?;
        transaction.commit().map_err(schema::map_sqlite)?;
        Ok(versioned(stored.value))
    }

    pub fn get_session(&self, id: &str) -> Result<Versioned<Session>, SessionError> {
        validate_session_id(id)?;
        let connection = self.connection()?;
        current_session(&connection, id)?
            .map(|stored| versioned(stored.value))
            .ok_or(SessionError::NotFound)
    }

    pub fn update_session(
        &self,
        id: &str,
        expected_etag: &str,
        patch: &SessionPatch,
    ) -> Result<Versioned<Session>, SessionError> {
        validate_session_id(id)?;
        if matches!(patch.title, PatchField::Missing)
            && matches!(patch.archived, PatchField::Missing)
        {
            return Err(SessionError::InvalidRequest);
        }
        let requested_title = match &patch.title {
            PatchField::Missing => None,
            PatchField::Value(value) => Some(normalize_title(value)?),
        };
        let ready = self.ready()?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| SessionError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)?;
        let current = current_session_tx(&transaction, id)?.ok_or(SessionError::NotFound)?;
        ensure_revision(&current.value.revision, expected_etag)?;

        let title = requested_title.unwrap_or_else(|| current.value.title.clone());
        let archived = match patch.archived {
            PatchField::Missing => current.value.archived,
            PatchField::Value(value) => value,
        };
        if title == current.value.title && archived == current.value.archived {
            transaction.commit().map_err(schema::map_sqlite)?;
            return Ok(versioned(current.value));
        }

        let change = next_change(&transaction)?;
        let revision = new_revision();
        let updated_at = next_timestamp(&current.value.updated_at)?;
        close_current_version(&transaction, id, change)?;
        transaction
            .execute(
                "UPDATE sessions SET title = ?1, archived = ?2, revision = ?3, \
                    updated_at = ?4, current_change = ?5 WHERE id = ?6",
                params![
                    title,
                    bool_to_i64(archived),
                    revision,
                    updated_at,
                    change,
                    id
                ],
            )
            .map_err(schema::map_sqlite)?;
        insert_current_version(&transaction, id, change)?;
        let updated = current_session_tx(&transaction, id)?.ok_or(SessionError::DataInvalid)?;
        transaction.commit().map_err(schema::map_sqlite)?;
        Ok(versioned(updated.value))
    }

    pub fn delete_session(
        &self,
        id: &str,
        expected_etag: Option<&str>,
    ) -> Result<(), SessionError> {
        validate_session_id(id)?;
        let ready = self.ready()?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| SessionError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)?;
        let Some(current) = current_session_tx(&transaction, id)? else {
            transaction.commit().map_err(schema::map_sqlite)?;
            return Ok(());
        };
        let expected_etag = expected_etag.ok_or(SessionError::PreconditionRequired)?;
        ensure_revision(&current.value.revision, expected_etag)?;
        let has_active_run: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id = ?1 AND status IN (\
                    'queued', 'running', 'waitingApproval', 'waitingClarification', 'cancelling'\
                 ))",
                [id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)?;
        if has_active_run {
            return Err(SessionError::Busy);
        }
        let change = next_change(&transaction)?;
        close_current_version(&transaction, id, change)?;
        transaction
            .execute("DELETE FROM sessions WHERE id = ?1", [id])
            .map_err(schema::map_sqlite)?;
        transaction.commit().map_err(schema::map_sqlite)
    }

    pub fn list_sessions(&self, request: &ListSessions) -> Result<SessionPage, SessionError> {
        validate_profile_id(&request.profile_id)?;
        if !(1..=100).contains(&request.limit) {
            return Err(SessionError::InvalidRequest);
        }
        let query = normalize_query(request.query.as_deref())?;
        if query.is_some() && self.search_mode() == SearchMode::Unavailable {
            return Err(SessionError::SearchUnavailable);
        }
        let filter_hash = CursorCodec::filter_hash(&[
            &request.profile_id,
            if request.archived { "true" } else { "false" },
            query.as_deref().unwrap_or(""),
        ]);
        let connection = self.connection()?;
        let (snapshot, before_updated_at, before_id) = match request.cursor.as_deref() {
            Some(cursor) => {
                let decoded = self.cursors.decode(cursor, "sessions", &filter_hash)?;
                if decoded.before_sequence.is_some()
                    || decoded.before_updated_at.is_none()
                    || decoded.before_id.is_none()
                {
                    return Err(SessionError::InvalidCursor);
                }
                (
                    decoded.snapshot,
                    decoded.before_updated_at,
                    decoded.before_id,
                )
            }
            None => (current_change(&connection)?, None, None),
        };
        let matching_messages = match query.as_deref() {
            Some(query) => {
                matching_message_session_ids(&connection, query, snapshot, self.search_mode())?
            }
            None => HashSet::new(),
        };
        let mut statement = connection
            .prepare(
                "SELECT session_id, profile_id, title, preview, source, model, message_count, \
                    archived, revision, created_at, updated_at, persona_id \
                 FROM session_versions \
                 WHERE profile_id = ?1 AND archived = ?2 \
                   AND valid_from_change <= ?3 \
                   AND (valid_to_change IS NULL OR valid_to_change > ?3) \
                 ORDER BY updated_at DESC, session_id DESC",
            )
            .map_err(schema::map_sqlite)?;
        let rows = statement
            .query_map(
                params![request.profile_id, bool_to_i64(request.archived), snapshot],
                session_version_from_row,
            )
            .map_err(schema::map_sqlite)?;
        let mut candidates = Vec::new();
        for row in rows {
            let mut session = row.map_err(schema::map_sqlite)?;
            if !is_after_position(&session, before_updated_at.as_deref(), before_id.as_deref()) {
                continue;
            }
            if let Some(query) = query.as_deref() {
                session.search_match = build_search_match(
                    &connection,
                    &session,
                    query,
                    snapshot,
                    matching_messages.contains(&session.id),
                    self.search_mode(),
                )?;
                if session.search_match.is_none() {
                    continue;
                }
            }
            candidates.push(session);
            if candidates.len() > request.limit {
                break;
            }
        }

        let has_more = candidates.len() > request.limit;
        candidates.truncate(request.limit);
        let next_cursor = if has_more {
            let last = candidates.last().ok_or(SessionError::DataInvalid)?;
            Some(self.cursors.encode(CursorPayload {
                version: 0,
                kind: "sessions".to_owned(),
                filter_hash,
                snapshot,
                before_updated_at: Some(last.updated_at.clone()),
                before_id: Some(last.id.clone()),
                before_sequence: None,
                issued_at: 0,
            })?)
        } else {
            None
        };
        Ok(SessionPage {
            items: candidates,
            next_cursor,
        })
    }

    pub fn commit_message(
        &self,
        session_id: &str,
        request: &CommitMessage,
    ) -> Result<(Message, Versioned<Session>), SessionError> {
        validate_session_id(session_id)?;
        validate_message(request)?;
        let parts_json =
            serde_json::to_string(&request.parts).map_err(|_| SessionError::DataInvalid)?;
        let tool_calls_json =
            serde_json::to_string(&request.tool_calls).map_err(|_| SessionError::DataInvalid)?;
        let searchable_text = searchable_text(&request.parts);
        let ready = self.ready()?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| SessionError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)?;
        let current =
            current_session_tx(&transaction, session_id)?.ok_or(SessionError::NotFound)?;
        if current.value.archived {
            return Err(SessionError::Archived);
        }
        let change = next_change(&transaction)?;
        let message_id = format!("message_{}", Uuid::new_v4().simple());
        let created_at = next_timestamp(&current.value.updated_at)?;
        let sequence = current.next_message_sequence;
        transaction
            .execute(
                "INSERT INTO messages(\
                    id, session_id, sequence, role, parts_json, reasoning, tool_calls_json, \
                    searchable_text, created_at, committed_change\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    message_id,
                    session_id,
                    i64::try_from(sequence).map_err(|_| SessionError::DataInvalid)?,
                    request.role.as_str(),
                    parts_json,
                    request.reasoning,
                    tool_calls_json,
                    searchable_text,
                    created_at,
                    change,
                ],
            )
            .map_err(schema::map_sqlite)?;
        if let Some(usage) = &request.usage {
            transaction
                .execute(
                    "INSERT INTO message_usage(\
                        message_id, prompt_tokens, completion_tokens, total_tokens, cost\
                     ) VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        message_id,
                        i64::try_from(usage.prompt_tokens)
                            .map_err(|_| SessionError::DataInvalid)?,
                        i64::try_from(usage.completion_tokens)
                            .map_err(|_| SessionError::DataInvalid)?,
                        i64::try_from(usage.total_tokens).map_err(|_| SessionError::DataInvalid)?,
                        usage.cost,
                    ],
                )
                .map_err(schema::map_sqlite)?;
        }

        let preview = if searchable_text.is_empty() {
            current.value.preview.clone()
        } else {
            truncate_chars(&searchable_text, MAX_PREVIEW_CHARS)
        };
        let model = request
            .model
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&current.value.model);
        let revision = new_revision();
        close_current_version(&transaction, session_id, change)?;
        transaction
            .execute(
                "UPDATE sessions SET preview = ?1, model = ?2, message_count = message_count + 1, \
                    revision = ?3, updated_at = ?4, next_message_sequence = next_message_sequence + 1, \
                    current_change = ?5 WHERE id = ?6",
                params![preview, model, revision, created_at, change, session_id],
            )
            .map_err(schema::map_sqlite)?;
        insert_current_version(&transaction, session_id, change)?;
        let message =
            message_by_id_tx(&transaction, &message_id)?.ok_or(SessionError::DataInvalid)?;
        let updated =
            current_session_tx(&transaction, session_id)?.ok_or(SessionError::DataInvalid)?;
        transaction.commit().map_err(schema::map_sqlite)?;
        Ok((message, versioned(updated.value)))
    }

    pub fn list_messages(
        &self,
        session_id: &str,
        request: &ListMessages,
    ) -> Result<MessagePage, SessionError> {
        validate_session_id(session_id)?;
        if !(1..=100).contains(&request.limit) {
            return Err(SessionError::InvalidRequest);
        }
        let connection = self.connection()?;
        if current_session(&connection, session_id)?.is_none() {
            return Err(SessionError::NotFound);
        }
        let filter_hash = CursorCodec::filter_hash(&[session_id]);
        let (snapshot, before_sequence) = match request.cursor.as_deref() {
            Some(cursor) => {
                let decoded = self.cursors.decode(cursor, "messages", &filter_hash)?;
                if decoded.before_updated_at.is_some()
                    || decoded.before_id.is_some()
                    || decoded.before_sequence.is_none()
                {
                    return Err(SessionError::InvalidCursor);
                }
                let snapshot =
                    u64::try_from(decoded.snapshot).map_err(|_| SessionError::InvalidCursor)?;
                (
                    snapshot,
                    decoded.before_sequence.ok_or(SessionError::InvalidCursor)?,
                )
            }
            None => {
                let snapshot: i64 = connection
                    .query_row(
                        "SELECT COALESCE(MAX(sequence), 0) FROM messages WHERE session_id = ?1",
                        [session_id],
                        |row| row.get(0),
                    )
                    .map_err(schema::map_sqlite)?;
                let snapshot = u64::try_from(snapshot).map_err(|_| SessionError::DataInvalid)?;
                (snapshot, snapshot.saturating_add(1))
            }
        };
        if before_sequence == 0 || before_sequence > snapshot.saturating_add(1) {
            return Err(SessionError::InvalidCursor);
        }
        let mut statement = connection
            .prepare(
                "SELECT m.id, m.session_id, m.sequence, m.role, m.parts_json, m.reasoning, \
                    m.tool_calls_json, m.created_at, u.prompt_tokens, u.completion_tokens, \
                    u.total_tokens, u.cost \
                 FROM messages m LEFT JOIN message_usage u ON u.message_id = m.id \
                 WHERE m.session_id = ?1 AND m.sequence <= ?2 AND m.sequence < ?3 \
                 ORDER BY m.sequence DESC LIMIT ?4",
            )
            .map_err(schema::map_sqlite)?;
        let query_limit =
            i64::try_from(request.limit + 1).map_err(|_| SessionError::InvalidRequest)?;
        let rows = statement
            .query_map(
                params![
                    session_id,
                    i64::try_from(snapshot).map_err(|_| SessionError::DataInvalid)?,
                    i64::try_from(before_sequence).map_err(|_| SessionError::InvalidCursor)?,
                    query_limit,
                ],
                message_from_row,
            )
            .map_err(schema::map_sqlite)?;
        let mut descending = Vec::new();
        for row in rows {
            descending.push(row.map_err(|_| SessionError::DataInvalid)?);
        }
        let has_more = descending.len() > request.limit;
        descending.truncate(request.limit);
        descending.reverse();
        let first_sequence = descending.first().map(|message| message.sequence);
        let last_sequence = descending.last().map(|message| message.sequence);
        let next_cursor = if has_more {
            let before_sequence = first_sequence.ok_or(SessionError::DataInvalid)?;
            Some(self.cursors.encode(CursorPayload {
                version: 0,
                kind: "messages".to_owned(),
                filter_hash,
                snapshot: i64::try_from(snapshot).map_err(|_| SessionError::DataInvalid)?,
                before_updated_at: None,
                before_id: None,
                before_sequence: Some(before_sequence),
                issued_at: 0,
            })?)
        } else {
            None
        };
        Ok(MessagePage {
            items: descending,
            next_cursor,
            snapshot_last_sequence: snapshot,
            first_sequence,
            last_sequence,
        })
    }

    pub(super) fn ready(&self) -> Result<&StorageReady, SessionError> {
        match &self.storage {
            StorageState::Ready(ready) => Ok(ready),
            StorageState::Unavailable => Err(SessionError::StorageUnavailable),
        }
    }

    fn connection(&self) -> Result<Connection, SessionError> {
        schema::open(&self.ready()?.db_path)
    }
}

fn current_session(
    connection: &Connection,
    id: &str,
) -> Result<Option<StoredSession>, SessionError> {
    connection
        .query_row(current_session_sql(), [id], stored_session_from_row)
        .optional()
        .map_err(schema::map_sqlite)
}

pub(super) fn current_session_tx(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<Option<StoredSession>, SessionError> {
    transaction
        .query_row(current_session_sql(), [id], stored_session_from_row)
        .optional()
        .map_err(schema::map_sqlite)
}

fn current_session_sql() -> &'static str {
    "SELECT id, profile_id, title, preview, source, model, message_count, archived, \
        revision, created_at, updated_at, next_message_sequence, persona_id \
     FROM sessions WHERE id = ?1"
}

fn stored_session_from_row(row: &Row<'_>) -> rusqlite::Result<StoredSession> {
    let message_count: i64 = row.get(6)?;
    let archived: i64 = row.get(7)?;
    let next_message_sequence: i64 = row.get(11)?;
    Ok(StoredSession {
        value: Session {
            id: row.get(0)?,
            profile_id: row.get(1)?,
            persona_id: row.get(12)?,
            title: row.get(2)?,
            preview: row.get(3)?,
            source: row.get(4)?,
            model: row.get(5)?,
            message_count: u64::try_from(message_count)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, message_count))?,
            archived: archived != 0,
            revision: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
            search_match: None,
        },
        next_message_sequence: u64::try_from(next_message_sequence)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, next_message_sequence))?,
    })
}

pub(super) fn message_by_id_tx(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<Option<Message>, SessionError> {
    transaction
        .query_row(
            "SELECT m.id, m.session_id, m.sequence, m.role, m.parts_json, m.reasoning, \
                m.tool_calls_json, m.created_at, u.prompt_tokens, u.completion_tokens, \
                u.total_tokens, u.cost \
             FROM messages m LEFT JOIN message_usage u ON u.message_id = m.id WHERE m.id = ?1",
            [id],
            message_from_row,
        )
        .optional()
        .map_err(|_| SessionError::DataInvalid)
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let sequence: i64 = row.get(2)?;
    let role: String = row.get(3)?;
    let parts_json: String = row.get(4)?;
    let tool_calls_json: String = row.get(6)?;
    let parts: Vec<MessagePart> = serde_json::from_str(&parts_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let tool_calls: Vec<ToolCall> = serde_json::from_str(&tool_calls_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let role = match role.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" => MessageRole::Tool,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                "invalid message role".into(),
            ));
        }
    };
    let prompt_tokens: Option<i64> = row.get(8)?;
    let usage = match prompt_tokens {
        Some(prompt_tokens) => {
            let completion_tokens: i64 = row.get(9)?;
            let total_tokens: i64 = row.get(10)?;
            Some(Usage {
                prompt_tokens: u64::try_from(prompt_tokens)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, prompt_tokens))?,
                completion_tokens: u64::try_from(completion_tokens)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, completion_tokens))?,
                total_tokens: u64::try_from(total_tokens)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, total_tokens))?,
                cost: row.get(11)?,
            })
        }
        None => None,
    };
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        sequence: u64::try_from(sequence)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, sequence))?,
        role,
        parts,
        reasoning: row.get(5)?,
        tool_calls,
        usage,
        created_at: row.get(7)?,
    })
}

fn session_version_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    let message_count: i64 = row.get(6)?;
    let archived: i64 = row.get(7)?;
    Ok(Session {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        persona_id: row.get(11)?,
        title: row.get(2)?,
        preview: row.get(3)?,
        source: row.get(4)?,
        model: row.get(5)?,
        message_count: u64::try_from(message_count)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, message_count))?,
        archived: archived != 0,
        revision: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        search_match: None,
    })
}

fn current_change(connection: &Connection) -> Result<i64, SessionError> {
    connection
        .query_row(
            "SELECT integer_value FROM app_meta WHERE key = 'session_change_sequence'",
            [],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
}

fn is_after_position(
    session: &Session,
    before_updated_at: Option<&str>,
    before_id: Option<&str>,
) -> bool {
    match (before_updated_at, before_id) {
        (Some(updated_at), Some(id)) => {
            session.updated_at.as_str() < updated_at
                || (session.updated_at == updated_at && session.id.as_str() < id)
        }
        (None, None) => true,
        _ => false,
    }
}

fn matching_message_session_ids(
    connection: &Connection,
    query: &str,
    snapshot: i64,
    mode: SearchMode,
) -> Result<HashSet<String>, SessionError> {
    let mut result = HashSet::new();
    if mode == SearchMode::Fts5 {
        let expression = fts_literal_expression(query);
        if let Ok(mut statement) = connection.prepare(
            "SELECT DISTINCT m.session_id FROM message_fts f \
             JOIN messages m ON m.id = f.message_id \
             WHERE message_fts MATCH ?1 AND m.committed_change <= ?2",
        ) && let Ok(rows) =
            statement.query_map(params![expression, snapshot], |row| row.get::<_, String>(0))
        {
            for id in rows.flatten() {
                result.insert(id);
            }
        }
    }

    let pattern = format!("%{}%", escape_like(query));
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT session_id FROM messages \
             WHERE committed_change <= ?1 \
               AND searchable_text LIKE ?2 ESCAPE '\\' COLLATE NOCASE",
        )
        .map_err(schema::map_sqlite)?;
    let rows = statement
        .query_map(params![snapshot, pattern], |row| row.get::<_, String>(0))
        .map_err(schema::map_sqlite)?;
    for row in rows {
        result.insert(row.map_err(schema::map_sqlite)?);
    }
    Ok(result)
}

fn build_search_match(
    connection: &Connection,
    session: &Session,
    query: &str,
    snapshot: i64,
    message_candidate: bool,
    mode: SearchMode,
) -> Result<Option<SearchMatch>, SessionError> {
    if let Some((snippet, ranges)) = snippet_and_ranges(&session.title, query) {
        return Ok(Some(SearchMatch {
            field: SearchField::Title,
            message_id: None,
            snippet,
            ranges,
        }));
    }
    if let Some((snippet, ranges)) = snippet_and_ranges(&session.id, query) {
        return Ok(Some(SearchMatch {
            field: SearchField::Id,
            message_id: None,
            snippet,
            ranges,
        }));
    }
    if !message_candidate {
        return Ok(None);
    }

    let pattern = format!("%{}%", escape_like(query));
    let literal = connection
        .query_row(
            "SELECT id, searchable_text FROM messages \
             WHERE session_id = ?1 AND committed_change <= ?2 \
               AND searchable_text LIKE ?3 ESCAPE '\\' COLLATE NOCASE \
             ORDER BY sequence DESC LIMIT 1",
            params![session.id, snapshot, pattern],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(schema::map_sqlite)?;
    let matched = if literal.is_some() || mode != SearchMode::Fts5 {
        literal
    } else {
        let expression = fts_literal_expression(query);
        connection
            .query_row(
                "SELECT m.id, m.searchable_text FROM message_fts f \
                 JOIN messages m ON m.id = f.message_id \
                 WHERE m.session_id = ?1 AND m.committed_change <= ?2 \
                   AND message_fts MATCH ?3 \
                 ORDER BY m.sequence DESC LIMIT 1",
                params![session.id, snapshot, expression],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .unwrap_or(None)
    };
    let Some((message_id, text)) = matched else {
        return Ok(None);
    };
    let (snippet, ranges) = snippet_and_ranges(&text, query)
        .unwrap_or_else(|| (truncate_chars(&text, MAX_PREVIEW_CHARS), Vec::new()));
    Ok(Some(SearchMatch {
        field: SearchField::Message,
        message_id: Some(message_id),
        snippet,
        ranges,
    }))
}

fn snippet_and_ranges(text: &str, query: &str) -> Option<(String, Vec<SearchRange>)> {
    let (start_byte, end_byte) = literal_byte_range(text, query)?;
    let start_char = text[..start_byte].chars().count();
    let end_char = start_char + text[start_byte..end_byte].chars().count();
    let total_chars = text.chars().count();
    let mut window_start = start_char.saturating_sub(120);
    if total_chars.saturating_sub(window_start) < MAX_PREVIEW_CHARS {
        window_start = total_chars.saturating_sub(MAX_PREVIEW_CHARS);
    }
    let window_end = (window_start + MAX_PREVIEW_CHARS).min(total_chars);
    let snippet: String = text
        .chars()
        .skip(window_start)
        .take(window_end - window_start)
        .collect();
    let range_prefix: String = text
        .chars()
        .skip(window_start)
        .take(start_char - window_start)
        .collect();
    let range_text: String = text
        .chars()
        .skip(start_char)
        .take(end_char - start_char)
        .collect();
    let start = range_prefix.encode_utf16().count();
    let end = start + range_text.encode_utf16().count();
    Some((snippet, vec![SearchRange { start, end }]))
}

fn literal_byte_range(text: &str, query: &str) -> Option<(usize, usize)> {
    if let Some(start) = text.find(query) {
        return Some((start, start + query.len()));
    }
    if text.is_ascii() && query.is_ascii() {
        let start = text
            .to_ascii_lowercase()
            .find(&query.to_ascii_lowercase())?;
        return Some((start, start + query.len()));
    }
    None
}

fn normalize_query(value: Option<&str>) -> Result<Option<String>, SessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let normalized = value.trim();
    if normalized.is_empty() {
        return Ok(None);
    }
    if normalized.chars().count() > MAX_QUERY_CHARS || normalized.chars().any(char::is_control) {
        return Err(SessionError::InvalidRequest);
    }
    Ok(Some(normalized.to_owned()))
}

fn escape_like(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            result.push('\\');
        }
        result.push(character);
    }
    result
}

fn fts_literal_expression(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn truncate_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

pub(super) fn validate_message(request: &CommitMessage) -> Result<(), SessionError> {
    if request.parts.is_empty()
        && request.reasoning.as_deref().unwrap_or("").is_empty()
        && request.tool_calls.is_empty()
    {
        return Err(SessionError::InvalidRequest);
    }
    for part in &request.parts {
        match part {
            MessagePart::Text { text } => {
                if text.chars().count() > MAX_MESSAGE_TEXT_CHARS {
                    return Err(SessionError::InvalidRequest);
                }
            }
            MessagePart::File {
                file_id,
                name,
                mime_type,
            } => {
                if file_id.is_empty()
                    || file_id.len() > 256
                    || name.is_empty()
                    || name.chars().count() > 500
                    || mime_type.is_empty()
                    || mime_type.len() > 255
                    || file_id.chars().any(char::is_control)
                    || name.chars().any(char::is_control)
                    || mime_type.chars().any(char::is_control)
                {
                    return Err(SessionError::InvalidRequest);
                }
            }
        }
    }
    if request
        .reasoning
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_MESSAGE_TEXT_CHARS)
    {
        return Err(SessionError::InvalidRequest);
    }
    for tool in &request.tool_calls {
        if tool.call_id.is_empty()
            || tool.call_id.len() > 256
            || tool.name.is_empty()
            || tool.name.chars().count() > 256
            || tool.call_id.chars().any(char::is_control)
            || tool.name.chars().any(char::is_control)
        {
            return Err(SessionError::InvalidRequest);
        }
    }
    if let Some(usage) = &request.usage
        && (!usage
            .cost
            .is_none_or(|cost| cost.is_finite() && cost >= 0.0)
            || usage.total_tokens < usage.prompt_tokens.saturating_add(usage.completion_tokens))
    {
        return Err(SessionError::InvalidRequest);
    }
    if request.model.as_deref().is_some_and(|model| {
        model.is_empty() || model.chars().count() > 500 || model.chars().any(char::is_control)
    }) {
        return Err(SessionError::InvalidRequest);
    }
    Ok(())
}

pub(super) fn searchable_text(parts: &[MessagePart]) -> String {
    let mut result = String::new();
    for part in parts {
        if let MessagePart::Text { text } = part {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(text);
        }
    }
    result
}

pub(super) fn next_change(transaction: &Transaction<'_>) -> Result<i64, SessionError> {
    transaction
        .execute(
            "UPDATE app_meta SET integer_value = integer_value + 1 \
             WHERE key = 'session_change_sequence'",
            [],
        )
        .map_err(schema::map_sqlite)?;
    transaction
        .query_row(
            "SELECT integer_value FROM app_meta WHERE key = 'session_change_sequence'",
            [],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
}

pub(super) fn insert_current_version(
    transaction: &Transaction<'_>,
    id: &str,
    valid_from_change: i64,
) -> Result<(), SessionError> {
    transaction
        .execute(
            "INSERT INTO session_versions(\
                session_id, valid_from_change, valid_to_change, profile_id, title, preview, \
                source, model, message_count, archived, revision, created_at, updated_at, persona_id\
             ) SELECT id, ?1, NULL, profile_id, title, preview, source, model, message_count, \
                archived, revision, created_at, updated_at, persona_id FROM sessions WHERE id = ?2",
            params![valid_from_change, id],
        )
        .map_err(schema::map_sqlite)?;
    Ok(())
}

pub(super) fn close_current_version(
    transaction: &Transaction<'_>,
    id: &str,
    valid_to_change: i64,
) -> Result<(), SessionError> {
    let changed = transaction
        .execute(
            "UPDATE session_versions SET valid_to_change = ?1 \
             WHERE session_id = ?2 AND valid_to_change IS NULL",
            params![valid_to_change, id],
        )
        .map_err(schema::map_sqlite)?;
    if changed != 1 {
        return Err(SessionError::DataInvalid);
    }
    Ok(())
}

fn ensure_revision(revision: &str, expected_etag: &str) -> Result<(), SessionError> {
    let current_etag = etag(revision);
    if expected_etag == current_etag {
        Ok(())
    } else {
        Err(SessionError::RevisionConflict { current_etag })
    }
}

fn versioned(mut session: Session) -> Versioned<Session> {
    session.search_match = None;
    let etag = etag(&session.revision);
    Versioned {
        value: session,
        etag,
    }
}

fn etag(revision: &str) -> String {
    format!("\"{revision}\"")
}

pub(super) fn new_revision() -> String {
    format!("session_rev_{}", Uuid::new_v4().simple())
}

pub(super) fn now_timestamp() -> Result<String, SessionError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| SessionError::DataInvalid)
}

pub(super) fn next_timestamp(previous: &str) -> Result<String, SessionError> {
    let previous =
        OffsetDateTime::parse(previous, &Rfc3339).map_err(|_| SessionError::DataInvalid)?;
    let now = OffsetDateTime::now_utc();
    let next = if now > previous {
        now
    } else {
        previous + Duration::nanoseconds(1)
    };
    next.format(&Rfc3339).map_err(|_| SessionError::DataInvalid)
}

fn validate_profile_id(value: &str) -> Result<(), SessionError> {
    if value == "default" || is_named_profile_id(value) {
        Ok(())
    } else {
        Err(SessionError::InvalidRequest)
    }
}

fn is_named_profile_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit() || first == b'_')
        && value.len() <= 64
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn validate_session_id(value: &str) -> Result<(), SessionError> {
    if value.is_empty()
        || value.len() > MAX_SESSION_ID_BYTES
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        Err(SessionError::InvalidSessionId)
    } else {
        Ok(())
    }
}

fn validate_optional_persona_id(value: Option<&str>) -> Result<(), SessionError> {
    if value.is_none_or(|value| {
        value.len() == 40
            && value.starts_with("persona_")
            && value[8..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) {
        Ok(())
    } else {
        Err(SessionError::InvalidRequest)
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), SessionError> {
    if (8..=128).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        Ok(())
    } else {
        Err(SessionError::InvalidRequest)
    }
}

fn normalize_optional_title(value: Option<&str>) -> Result<String, SessionError> {
    match value {
        Some(value) => normalize_title(value),
        None => Ok(DEFAULT_TITLE.to_owned()),
    }
}

fn normalize_title(value: &str) -> Result<String, SessionError> {
    let normalized = value.trim();
    if normalized.is_empty()
        || normalized.chars().count() > MAX_TITLE_CHARS
        || normalized.chars().any(char::is_control)
    {
        Err(SessionError::InvalidRequest)
    } else {
        Ok(normalized.to_owned())
    }
}

fn create_fingerprint(profile_id: &str, title: &str, persona_id: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-create-session-v1\0");
    digest.update((profile_id.len() as u64).to_be_bytes());
    digest.update(profile_id.as_bytes());
    digest.update((title.len() as u64).to_be_bytes());
    digest.update(title.as_bytes());
    if let Some(persona_id) = persona_id {
        digest.update(b"\0persona\0");
        digest.update((persona_id.len() as u64).to_be_bytes());
        digest.update(persona_id.as_bytes());
    }
    hex(&digest.finalize())
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

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}
