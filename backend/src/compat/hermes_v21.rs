//! Read-only adapter for the `state.db` schema at the pinned Hermes Agent commit.
//!
//! This module performs no migrations, repairs, checkpoints, or FTS reads. Its
//! output is an import-neutral snapshot: attachment references and billing base
//! URLs are represented only by digests, and diagnostics never include the
//! source database path or malformed payload text.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

pub const HERMES_V21_SCHEMA_VERSION: i64 = 21;
pub const HERMES_AGENT_COMMIT: &str = "3f2a389c7e1f1729cad91ae63c26fb08c7753c74";
pub const HERMES_AGENT_REPOSITORY: &str = "https://github.com/NousResearch/hermes-agent.git";
pub const HERMES_V21_ADAPTER_ID: &str = "hermes-agent-state-v21";

const MAX_WARNINGS: usize = 256;
const MAX_SESSIONS: i64 = 50_000;
const MAX_MESSAGES: i64 = 250_000;
const MAX_MODEL_USAGE_ROWS: i64 = 100_000;
const MAX_STRUCTURED_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUXILIARY_JSON_BYTES: usize = 4 * 1024 * 1024;
const CONTENT_JSON_PREFIX: &str = "\0json:";

const SCHEMA_VERSION_COLUMNS: &[&str] = &["version"];
const SESSION_COLUMNS: &[&str] = &[
    "id",
    "source",
    "model",
    "parent_session_id",
    "started_at",
    "ended_at",
    "message_count",
    "tool_call_count",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "estimated_cost_usd",
    "actual_cost_usd",
    "title",
    "api_call_count",
    "archived",
];
const MESSAGE_COLUMNS: &[&str] = &[
    "id",
    "session_id",
    "role",
    "content",
    "tool_call_id",
    "tool_calls",
    "tool_name",
    "timestamp",
    "token_count",
    "finish_reason",
    "reasoning",
    "reasoning_content",
    "reasoning_details",
    "active",
    "compacted",
];
const MODEL_USAGE_COLUMNS: &[&str] = &[
    "session_id",
    "model",
    "billing_provider",
    "billing_base_url",
    "billing_mode",
    "api_call_count",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "estimated_cost_usd",
    "actual_cost_usd",
    "cost_status",
    "cost_source",
    "first_seen",
    "last_seen",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesV21Snapshot {
    pub provenance: SnapshotProvenance,
    pub statistics: SnapshotStatistics,
    pub sessions: Vec<HermesV21Session>,
    pub messages: Vec<HermesV21Message>,
    pub model_usage: Vec<HermesV21ModelUsage>,
    pub warnings: Vec<ImportWarning>,
    pub warnings_dropped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotStatistics {
    pub rewound_message_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotProvenance {
    pub adapter_id: String,
    pub upstream_repository: String,
    pub upstream_commit: String,
    pub schema_version: i64,
    pub logical_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordProvenance {
    pub table: SourceTable,
    pub source_key_digest: String,
    pub source_row_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTable {
    Sessions,
    Messages,
    SessionModelUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesV21Session {
    pub provenance: RecordProvenance,
    pub upstream_id: String,
    pub source: String,
    pub model: Option<String>,
    pub parent_upstream_id: Option<String>,
    pub title: Option<String>,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub archived: bool,
    pub aggregate_usage: SessionAggregateUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAggregateUsage {
    pub message_count: i64,
    pub tool_call_count: i64,
    pub api_call_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub estimated_cost_usd: Option<f64>,
    pub actual_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesV21Message {
    pub provenance: RecordProvenance,
    pub upstream_id: i64,
    pub session_upstream_id: String,
    pub role: String,
    pub content: ImportedContent,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_calls: Vec<ImportedToolCall>,
    pub timestamp: f64,
    pub token_count: Option<i64>,
    pub finish_reason: Option<String>,
    pub reasoning: Option<String>,
    pub active: bool,
    pub compacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedContent {
    pub text: Option<String>,
    pub pending_attachments: Vec<PendingAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAttachment {
    pub ordinal: usize,
    pub kind: PendingAttachmentKind,
    pub media_type: Option<String>,
    pub reference_kind: AttachmentReferenceKind,
    pub reference_fingerprint: Option<String>,
    pub import_state: AttachmentImportState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingAttachmentKind {
    Image,
    Audio,
    File,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentReferenceKind {
    EmbeddedData,
    RemoteUrl,
    LocalFile,
    Opaque,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentImportState {
    RequiresPolicyValidation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedToolCall {
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: JsonValue,
    pub arguments_format: ToolArgumentsFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolArgumentsFormat {
    Json,
    Text,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HermesV21ModelUsage {
    pub provenance: RecordProvenance,
    pub session_upstream_id: String,
    pub model: String,
    pub billing_provider: String,
    pub billing_mode: String,
    pub billing_base_url_present: bool,
    pub route_fingerprint: String,
    pub api_call_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub estimated_cost_usd: f64,
    pub actual_cost_usd: f64,
    pub cost_status: Option<String>,
    pub cost_source: Option<String>,
    pub first_seen: Option<f64>,
    pub last_seen: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWarning {
    pub code: WarningCode,
    pub table: SourceTable,
    pub record_number: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    ActiveNullTreatedAsActive,
    StructuredContentTooLarge,
    StructuredContentInvalidJson,
    StructuredContentUnsupportedShape,
    StructuredContentPartIgnored,
    AttachmentReferenceMissing,
    ReasoningDetailsTooLarge,
    ReasoningDetailsInvalidJson,
    ReasoningIgnoredForRole,
    ToolCallsTooLarge,
    ToolCallsInvalidJson,
    ToolCallsNotArray,
    ToolCallEntryIgnored,
    ToolCallArgumentsInvalidJson,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HermesV21Error {
    #[error("Hermes state database could not be opened in read-only mode")]
    OpenFailed,
    #[error("Hermes state database URI could not be constructed")]
    InvalidDatabaseLocation,
    #[error("Hermes state database read-only enforcement failed")]
    ReadOnlyEnforcementFailed,
    #[error("Hermes state database read transaction failed")]
    TransactionFailed,
    #[error("Hermes state database is missing required table `{table}`")]
    MissingTable { table: &'static str },
    #[error("Hermes state database is missing required column `{table}.{column}`")]
    MissingColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("Hermes state database has no schema version")]
    MissingSchemaVersion,
    #[error("Hermes state database has more than one schema version row")]
    AmbiguousSchemaVersion,
    #[error("Hermes state database schema version is not an integer")]
    InvalidSchemaVersion,
    #[error("unsupported Hermes state database schema version {found}; expected 21")]
    UnsupportedSchemaVersion { found: i64 },
    #[error("Hermes state database contains an invalid value in `{table}.{column}`")]
    InvalidValue {
        table: &'static str,
        column: &'static str,
    },
    #[error("Hermes state database snapshot fingerprint could not be calculated")]
    FingerprintFailed,
    #[error("Hermes state database exceeds the supported import limits")]
    SnapshotTooLarge,
}

#[derive(Default)]
struct WarningSink {
    warnings: Vec<ImportWarning>,
    dropped: usize,
}

impl WarningSink {
    fn push(&mut self, warning: ImportWarning) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(warning);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
}

/// Read one consistent v21 snapshot from an existing Hermes `state.db`.
///
/// The connection uses SQLite URI `mode=ro`, `query_only=ON`, and one deferred
/// read transaction. The returned DTO contains no source database path.
pub fn read_snapshot(path: impl AsRef<Path>) -> Result<HermesV21Snapshot, HermesV21Error> {
    let uri = read_only_uri(path.as_ref())?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut connection =
        Connection::open_with_flags(uri.as_str(), flags).map_err(|_| HermesV21Error::OpenFailed)?;

    connection
        .pragma_update(None, "query_only", true)
        .map_err(|_| HermesV21Error::ReadOnlyEnforcementFailed)?;
    let query_only = connection
        .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
        .map_err(|_| HermesV21Error::ReadOnlyEnforcementFailed)?;
    if query_only != 1 {
        return Err(HermesV21Error::ReadOnlyEnforcementFailed);
    }

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    validate_schema(&transaction)?;
    let statistics = validate_snapshot_size(&transaction)?;

    let mut warning_sink = WarningSink::default();
    let sessions = read_sessions(&transaction)?;
    let messages = read_messages(&transaction, &mut warning_sink)?;
    let model_usage = read_model_usage(&transaction)?;
    let logical_fingerprint = logical_fingerprint(&sessions, &messages, &model_usage)?;

    transaction
        .commit()
        .map_err(|_| HermesV21Error::TransactionFailed)?;

    Ok(HermesV21Snapshot {
        provenance: SnapshotProvenance {
            adapter_id: HERMES_V21_ADAPTER_ID.to_owned(),
            upstream_repository: HERMES_AGENT_REPOSITORY.to_owned(),
            upstream_commit: HERMES_AGENT_COMMIT.to_owned(),
            schema_version: HERMES_V21_SCHEMA_VERSION,
            logical_fingerprint,
        },
        statistics,
        sessions,
        messages,
        model_usage,
        warnings: warning_sink.warnings,
        warnings_dropped: warning_sink.dropped,
    })
}

fn validate_snapshot_size(
    transaction: &Transaction<'_>,
) -> Result<SnapshotStatistics, HermesV21Error> {
    let session_count = count_rows(transaction, "SELECT COUNT(*) FROM sessions")?;
    let message_count = count_rows(
        transaction,
        "SELECT COUNT(*) FROM messages \
         WHERE COALESCE(active, 1) = 1 OR COALESCE(compacted, 0) = 1",
    )?;
    let model_usage_count = count_rows(transaction, "SELECT COUNT(*) FROM session_model_usage")?;
    if session_count > MAX_SESSIONS
        || message_count > MAX_MESSAGES
        || model_usage_count > MAX_MODEL_USAGE_ROWS
    {
        return Err(HermesV21Error::SnapshotTooLarge);
    }
    let rewound_message_count = count_rows(
        transaction,
        "SELECT COUNT(*) FROM messages \
         WHERE COALESCE(active, 1) = 0 AND COALESCE(compacted, 0) = 0",
    )?;
    Ok(SnapshotStatistics {
        rewound_message_count: usize::try_from(rewound_message_count)
            .map_err(|_| HermesV21Error::SnapshotTooLarge)?,
    })
}

fn count_rows(transaction: &Transaction<'_>, sql: &str) -> Result<i64, HermesV21Error> {
    transaction
        .query_row(sql, [], |row| row.get(0))
        .map_err(|_| HermesV21Error::TransactionFailed)
}

fn read_only_uri(path: &Path) -> Result<Url, HermesV21Error> {
    let absolute = absolute_path(path)?;
    let mut uri =
        Url::from_file_path(absolute).map_err(|_| HermesV21Error::InvalidDatabaseLocation)?;
    uri.set_query(Some("mode=ro"));
    Ok(uri)
}

fn absolute_path(path: &Path) -> Result<PathBuf, HermesV21Error> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|_| HermesV21Error::InvalidDatabaseLocation)
}

fn validate_schema(transaction: &Transaction<'_>) -> Result<(), HermesV21Error> {
    ensure_columns(transaction, "schema_version", SCHEMA_VERSION_COLUMNS)?;
    let version = read_schema_version(transaction)?;
    if version != HERMES_V21_SCHEMA_VERSION {
        return Err(HermesV21Error::UnsupportedSchemaVersion { found: version });
    }

    ensure_columns(transaction, "sessions", SESSION_COLUMNS)?;
    ensure_columns(transaction, "messages", MESSAGE_COLUMNS)?;
    ensure_columns(transaction, "session_model_usage", MODEL_USAGE_COLUMNS)?;
    Ok(())
}

fn ensure_columns(
    transaction: &Transaction<'_>,
    table: &'static str,
    required: &'static [&'static str],
) -> Result<(), HermesV21Error> {
    let pragma = match table {
        "schema_version" => "PRAGMA table_info(\"schema_version\")",
        "sessions" => "PRAGMA table_info(\"sessions\")",
        "messages" => "PRAGMA table_info(\"messages\")",
        "session_model_usage" => "PRAGMA table_info(\"session_model_usage\")",
        _ => return Err(HermesV21Error::MissingTable { table }),
    };
    let mut statement = transaction
        .prepare(pragma)
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| HermesV21Error::TransactionFailed)?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|_| HermesV21Error::TransactionFailed)?;

    if columns.is_empty() {
        return Err(HermesV21Error::MissingTable { table });
    }
    for &column in required {
        if !columns.contains(column) {
            return Err(HermesV21Error::MissingColumn { table, column });
        }
    }
    Ok(())
}

fn read_schema_version(transaction: &Transaction<'_>) -> Result<i64, HermesV21Error> {
    let mut statement = transaction
        .prepare("SELECT version FROM schema_version LIMIT 2")
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut rows = statement
        .query([])
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let first = rows
        .next()
        .map_err(|_| HermesV21Error::TransactionFailed)?
        .ok_or(HermesV21Error::MissingSchemaVersion)?;
    let version = first
        .get::<_, i64>(0)
        .map_err(|_| HermesV21Error::InvalidSchemaVersion)?;
    if rows
        .next()
        .map_err(|_| HermesV21Error::TransactionFailed)?
        .is_some()
    {
        return Err(HermesV21Error::AmbiguousSchemaVersion);
    }
    Ok(version)
}

fn read_sessions(transaction: &Transaction<'_>) -> Result<Vec<HermesV21Session>, HermesV21Error> {
    const SQL: &str = r#"
        SELECT id, source, model, parent_session_id, title, started_at, ended_at,
               COALESCE(archived, 0), COALESCE(message_count, 0),
               COALESCE(tool_call_count, 0), COALESCE(api_call_count, 0),
               COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
               COALESCE(cache_read_tokens, 0), COALESCE(cache_write_tokens, 0),
               COALESCE(reasoning_tokens, 0), estimated_cost_usd, actual_cost_usd
          FROM sessions
         ORDER BY started_at ASC, id ASC
    "#;
    let mut statement = transaction
        .prepare(SQL)
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut rows = statement
        .query([])
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut sessions = Vec::new();

    while let Some(row) = rows.next().map_err(|_| HermesV21Error::TransactionFailed)? {
        let upstream_id: String = required_value(row, 0, "sessions", "id")?;
        let source: String = required_value(row, 1, "sessions", "source")?;
        let model: Option<String> = optional_value(row, 2, "sessions", "model")?;
        let parent_upstream_id: Option<String> =
            optional_value(row, 3, "sessions", "parent_session_id")?;
        let title: Option<String> = optional_value(row, 4, "sessions", "title")?;
        let started_at: f64 = required_value(row, 5, "sessions", "started_at")?;
        let ended_at: Option<f64> = optional_value(row, 6, "sessions", "ended_at")?;
        let archived: i64 = required_value(row, 7, "sessions", "archived")?;
        let message_count: i64 = required_value(row, 8, "sessions", "message_count")?;
        let tool_call_count: i64 = required_value(row, 9, "sessions", "tool_call_count")?;
        let api_call_count: i64 = required_value(row, 10, "sessions", "api_call_count")?;
        let input_tokens: i64 = required_value(row, 11, "sessions", "input_tokens")?;
        let output_tokens: i64 = required_value(row, 12, "sessions", "output_tokens")?;
        let cache_read_tokens: i64 = required_value(row, 13, "sessions", "cache_read_tokens")?;
        let cache_write_tokens: i64 = required_value(row, 14, "sessions", "cache_write_tokens")?;
        let reasoning_tokens: i64 = required_value(row, 15, "sessions", "reasoning_tokens")?;
        let estimated_cost_usd: Option<f64> =
            optional_value(row, 16, "sessions", "estimated_cost_usd")?;
        let actual_cost_usd: Option<f64> = optional_value(row, 17, "sessions", "actual_cost_usd")?;

        ensure_finite(started_at, "sessions", "started_at")?;
        ensure_optional_finite(ended_at, "sessions", "ended_at")?;
        ensure_optional_finite(estimated_cost_usd, "sessions", "estimated_cost_usd")?;
        ensure_optional_finite(actual_cost_usd, "sessions", "actual_cost_usd")?;

        let row_digest_value = serde_json::json!({
            "id": upstream_id,
            "source": source,
            "model": model,
            "parent_session_id": parent_upstream_id,
            "title": title,
            "started_at": started_at,
            "ended_at": ended_at,
            "archived": archived,
            "message_count": message_count,
            "tool_call_count": tool_call_count,
            "api_call_count": api_call_count,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_tokens": cache_read_tokens,
            "cache_write_tokens": cache_write_tokens,
            "reasoning_tokens": reasoning_tokens,
            "estimated_cost_usd": estimated_cost_usd,
            "actual_cost_usd": actual_cost_usd,
        });
        let provenance = record_provenance(
            SourceTable::Sessions,
            upstream_id.as_bytes(),
            &row_digest_value,
        )?;

        sessions.push(HermesV21Session {
            provenance,
            upstream_id,
            source,
            model,
            parent_upstream_id,
            title,
            started_at,
            ended_at,
            archived: strict_bool(archived, "sessions", "archived")?,
            aggregate_usage: SessionAggregateUsage {
                message_count,
                tool_call_count,
                api_call_count,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                estimated_cost_usd,
                actual_cost_usd,
            },
        });
    }

    Ok(sessions)
}

fn read_messages(
    transaction: &Transaction<'_>,
    warnings: &mut WarningSink,
) -> Result<Vec<HermesV21Message>, HermesV21Error> {
    const SQL: &str = r#"
        SELECT id, session_id, role, content, tool_call_id, tool_calls, tool_name,
               timestamp, token_count, finish_reason, reasoning,
               reasoning_content, reasoning_details, active, compacted
         FROM messages
         WHERE COALESCE(active, 1) = 1 OR COALESCE(compacted, 0) = 1
         ORDER BY timestamp ASC, id ASC
    "#;
    let mut statement = transaction
        .prepare(SQL)
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut rows = statement
        .query([])
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut messages = Vec::new();

    while let Some(row) = rows.next().map_err(|_| HermesV21Error::TransactionFailed)? {
        let upstream_id: i64 = required_value(row, 0, "messages", "id")?;
        let session_upstream_id: String = required_value(row, 1, "messages", "session_id")?;
        let role: String = required_value(row, 2, "messages", "role")?;
        let raw_content: Option<String> = optional_value(row, 3, "messages", "content")?;
        let tool_call_id: Option<String> = optional_value(row, 4, "messages", "tool_call_id")?;
        let raw_tool_calls: Option<String> = optional_value(row, 5, "messages", "tool_calls")?;
        let tool_name: Option<String> = optional_value(row, 6, "messages", "tool_name")?;
        let timestamp: f64 = required_value(row, 7, "messages", "timestamp")?;
        let token_count: Option<i64> = optional_value(row, 8, "messages", "token_count")?;
        let finish_reason: Option<String> = optional_value(row, 9, "messages", "finish_reason")?;
        let raw_reasoning: Option<String> = optional_value(row, 10, "messages", "reasoning")?;
        let raw_reasoning_content: Option<String> =
            optional_value(row, 11, "messages", "reasoning_content")?;
        let raw_reasoning_details: Option<String> =
            optional_value(row, 12, "messages", "reasoning_details")?;
        let raw_active: Option<i64> = optional_value(row, 13, "messages", "active")?;
        let compacted: i64 = required_value(row, 14, "messages", "compacted")?;
        ensure_finite(timestamp, "messages", "timestamp")?;
        let active = match raw_active {
            Some(value) => strict_bool(value, "messages", "active")?,
            None => {
                push_message_warning(
                    warnings,
                    upstream_id,
                    WarningCode::ActiveNullTreatedAsActive,
                );
                true
            }
        };
        let compacted = strict_bool(compacted, "messages", "compacted")?;

        let row_digest_value = serde_json::json!({
            "id": upstream_id,
            "session_id": session_upstream_id,
            "role": role,
            "content": raw_content,
            "tool_call_id": tool_call_id,
            "tool_calls": raw_tool_calls,
            "tool_name": tool_name,
            "timestamp": timestamp,
            "token_count": token_count,
            "finish_reason": finish_reason,
            "reasoning": raw_reasoning,
            "reasoning_content": raw_reasoning_content,
            "reasoning_details": raw_reasoning_details,
            "active": if active { 1 } else { 0 },
            "compacted": if compacted { 1 } else { 0 },
        });
        let provenance = record_provenance(
            SourceTable::Messages,
            upstream_id.to_string().as_bytes(),
            &row_digest_value,
        )?;
        let content = parse_content(raw_content.as_deref(), upstream_id, warnings);
        let tool_calls = parse_tool_calls(raw_tool_calls.as_deref(), upstream_id, warnings);
        let reasoning = pick_reasoning(
            &role,
            raw_reasoning.as_deref(),
            raw_reasoning_content.as_deref(),
            raw_reasoning_details.as_deref(),
            upstream_id,
            warnings,
        );

        messages.push(HermesV21Message {
            provenance,
            upstream_id,
            session_upstream_id,
            role,
            content,
            tool_call_id,
            tool_name,
            tool_calls,
            timestamp,
            token_count,
            finish_reason,
            reasoning,
            active,
            compacted,
        });
    }

    Ok(messages)
}

fn read_model_usage(
    transaction: &Transaction<'_>,
) -> Result<Vec<HermesV21ModelUsage>, HermesV21Error> {
    const SQL: &str = r#"
        SELECT session_id, model, billing_provider, billing_base_url, billing_mode,
               api_call_count, input_tokens, output_tokens, cache_read_tokens,
               cache_write_tokens, reasoning_tokens, estimated_cost_usd,
               actual_cost_usd, cost_status, cost_source, first_seen, last_seen
          FROM session_model_usage
         ORDER BY session_id ASC, model ASC, billing_provider ASC,
                  billing_base_url ASC, billing_mode ASC
    "#;
    let mut statement = transaction
        .prepare(SQL)
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut rows = statement
        .query([])
        .map_err(|_| HermesV21Error::TransactionFailed)?;
    let mut usage_rows = Vec::new();

    while let Some(row) = rows.next().map_err(|_| HermesV21Error::TransactionFailed)? {
        let session_upstream_id: String =
            required_value(row, 0, "session_model_usage", "session_id")?;
        let model: String = required_value(row, 1, "session_model_usage", "model")?;
        let billing_provider: String =
            required_value(row, 2, "session_model_usage", "billing_provider")?;
        let billing_base_url: String =
            required_value(row, 3, "session_model_usage", "billing_base_url")?;
        let billing_mode: String = required_value(row, 4, "session_model_usage", "billing_mode")?;
        let api_call_count: i64 = required_value(row, 5, "session_model_usage", "api_call_count")?;
        let input_tokens: i64 = required_value(row, 6, "session_model_usage", "input_tokens")?;
        let output_tokens: i64 = required_value(row, 7, "session_model_usage", "output_tokens")?;
        let cache_read_tokens: i64 =
            required_value(row, 8, "session_model_usage", "cache_read_tokens")?;
        let cache_write_tokens: i64 =
            required_value(row, 9, "session_model_usage", "cache_write_tokens")?;
        let reasoning_tokens: i64 =
            required_value(row, 10, "session_model_usage", "reasoning_tokens")?;
        let estimated_cost_usd: f64 =
            required_value(row, 11, "session_model_usage", "estimated_cost_usd")?;
        let actual_cost_usd: f64 =
            required_value(row, 12, "session_model_usage", "actual_cost_usd")?;
        let cost_status: Option<String> =
            optional_value(row, 13, "session_model_usage", "cost_status")?;
        let cost_source: Option<String> =
            optional_value(row, 14, "session_model_usage", "cost_source")?;
        let first_seen: Option<f64> = optional_value(row, 15, "session_model_usage", "first_seen")?;
        let last_seen: Option<f64> = optional_value(row, 16, "session_model_usage", "last_seen")?;

        ensure_finite(
            estimated_cost_usd,
            "session_model_usage",
            "estimated_cost_usd",
        )?;
        ensure_finite(actual_cost_usd, "session_model_usage", "actual_cost_usd")?;
        ensure_optional_finite(first_seen, "session_model_usage", "first_seen")?;
        ensure_optional_finite(last_seen, "session_model_usage", "last_seen")?;

        let route_fingerprint = digest_bytes(
            format!("{model}\0{billing_provider}\0{billing_base_url}\0{billing_mode}").as_bytes(),
        );
        let row_digest_value = serde_json::json!({
            "session_id": session_upstream_id,
            "model": model,
            "billing_provider": billing_provider,
            "billing_base_url": billing_base_url,
            "billing_mode": billing_mode,
            "api_call_count": api_call_count,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_tokens": cache_read_tokens,
            "cache_write_tokens": cache_write_tokens,
            "reasoning_tokens": reasoning_tokens,
            "estimated_cost_usd": estimated_cost_usd,
            "actual_cost_usd": actual_cost_usd,
            "cost_status": cost_status,
            "cost_source": cost_source,
            "first_seen": first_seen,
            "last_seen": last_seen,
        });
        let source_key = format!(
            "{session_upstream_id}\0{model}\0{billing_provider}\0{billing_base_url}\0{billing_mode}"
        );
        let provenance = record_provenance(
            SourceTable::SessionModelUsage,
            source_key.as_bytes(),
            &row_digest_value,
        )?;

        usage_rows.push(HermesV21ModelUsage {
            provenance,
            session_upstream_id,
            model,
            billing_provider,
            billing_mode,
            billing_base_url_present: !billing_base_url.is_empty(),
            route_fingerprint,
            api_call_count,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            estimated_cost_usd,
            actual_cost_usd,
            cost_status,
            cost_source,
            first_seen,
            last_seen,
        });
    }

    Ok(usage_rows)
}

fn required_value<T: rusqlite::types::FromSql>(
    row: &rusqlite::Row<'_>,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<T, HermesV21Error> {
    row.get(index)
        .map_err(|_| HermesV21Error::InvalidValue { table, column })
}

fn optional_value<T: rusqlite::types::FromSql>(
    row: &rusqlite::Row<'_>,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<T>, HermesV21Error> {
    row.get(index)
        .map_err(|_| HermesV21Error::InvalidValue { table, column })
}

fn ensure_finite(
    value: f64,
    table: &'static str,
    column: &'static str,
) -> Result<(), HermesV21Error> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(HermesV21Error::InvalidValue { table, column })
    }
}

fn ensure_optional_finite(
    value: Option<f64>,
    table: &'static str,
    column: &'static str,
) -> Result<(), HermesV21Error> {
    match value {
        Some(value) => ensure_finite(value, table, column),
        None => Ok(()),
    }
}

fn strict_bool(
    value: i64,
    table: &'static str,
    column: &'static str,
) -> Result<bool, HermesV21Error> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(HermesV21Error::InvalidValue { table, column }),
    }
}

fn parse_content(
    raw: Option<&str>,
    message_id: i64,
    warnings: &mut WarningSink,
) -> ImportedContent {
    let Some(raw) = raw else {
        return ImportedContent {
            text: None,
            pending_attachments: Vec::new(),
        };
    };
    let Some(encoded) = raw.strip_prefix(CONTENT_JSON_PREFIX) else {
        return ImportedContent {
            text: Some(raw.to_owned()),
            pending_attachments: Vec::new(),
        };
    };
    if encoded.len() > MAX_STRUCTURED_CONTENT_BYTES {
        push_message_warning(warnings, message_id, WarningCode::StructuredContentTooLarge);
        return ImportedContent {
            text: None,
            pending_attachments: Vec::new(),
        };
    }

    let parsed = match serde_json::from_str::<JsonValue>(encoded) {
        Ok(parsed) => parsed,
        Err(_) => {
            push_message_warning(
                warnings,
                message_id,
                WarningCode::StructuredContentInvalidJson,
            );
            return ImportedContent {
                text: None,
                pending_attachments: Vec::new(),
            };
        }
    };
    decode_structured_content(&parsed, message_id, warnings)
}

fn decode_structured_content(
    value: &JsonValue,
    message_id: i64,
    warnings: &mut WarningSink,
) -> ImportedContent {
    match value {
        JsonValue::String(text) => ImportedContent {
            text: Some(text.clone()),
            pending_attachments: Vec::new(),
        },
        JsonValue::Array(parts) => decode_content_parts(parts, message_id, warnings),
        JsonValue::Object(object) if object.get("_multimodal") == Some(&JsonValue::Bool(true)) => {
            let mut decoded = match object.get("content") {
                Some(JsonValue::Array(parts)) => decode_content_parts(parts, message_id, warnings),
                _ => {
                    push_message_warning(
                        warnings,
                        message_id,
                        WarningCode::StructuredContentUnsupportedShape,
                    );
                    ImportedContent {
                        text: None,
                        pending_attachments: Vec::new(),
                    }
                }
            };
            if decoded.text.is_none()
                && let Some(summary) = object.get("text_summary").and_then(JsonValue::as_str)
                && !summary.is_empty()
            {
                decoded.text = Some(summary.to_owned());
            }
            decoded
        }
        JsonValue::Object(object) => {
            if let Some(text) = object.get("text").and_then(JsonValue::as_str) {
                ImportedContent {
                    text: Some(text.to_owned()),
                    pending_attachments: Vec::new(),
                }
            } else {
                push_message_warning(
                    warnings,
                    message_id,
                    WarningCode::StructuredContentUnsupportedShape,
                );
                ImportedContent {
                    text: None,
                    pending_attachments: Vec::new(),
                }
            }
        }
        _ => {
            push_message_warning(
                warnings,
                message_id,
                WarningCode::StructuredContentUnsupportedShape,
            );
            ImportedContent {
                text: None,
                pending_attachments: Vec::new(),
            }
        }
    }
}

fn decode_content_parts(
    parts: &[JsonValue],
    message_id: i64,
    warnings: &mut WarningSink,
) -> ImportedContent {
    let mut text_parts = Vec::new();
    let mut pending_attachments = Vec::new();

    for part in parts {
        match part {
            JsonValue::String(text) if !text.is_empty() => text_parts.push(text.clone()),
            JsonValue::Object(object) => {
                let part_type = object
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                match part_type.as_str() {
                    "text" | "input_text" | "output_text" => {
                        if let Some(text) = object.get("text").and_then(JsonValue::as_str)
                            && !text.is_empty()
                        {
                            text_parts.push(text.to_owned());
                        }
                    }
                    "image_url" | "input_image" | "image" => {
                        pending_attachments.push(pending_attachment(
                            object,
                            part_type.as_str(),
                            PendingAttachmentKind::Image,
                            pending_attachments.len(),
                            message_id,
                            warnings,
                        ));
                    }
                    "input_audio" | "audio" => {
                        pending_attachments.push(pending_attachment(
                            object,
                            part_type.as_str(),
                            PendingAttachmentKind::Audio,
                            pending_attachments.len(),
                            message_id,
                            warnings,
                        ));
                    }
                    "file" | "input_file" | "document" => {
                        pending_attachments.push(pending_attachment(
                            object,
                            part_type.as_str(),
                            PendingAttachmentKind::File,
                            pending_attachments.len(),
                            message_id,
                            warnings,
                        ));
                    }
                    _ => push_message_warning(
                        warnings,
                        message_id,
                        WarningCode::StructuredContentPartIgnored,
                    ),
                }
            }
            JsonValue::Null => {}
            _ => push_message_warning(
                warnings,
                message_id,
                WarningCode::StructuredContentPartIgnored,
            ),
        }
    }

    ImportedContent {
        text: (!text_parts.is_empty()).then(|| text_parts.join("\n\n")),
        pending_attachments,
    }
}

fn pending_attachment(
    object: &serde_json::Map<String, JsonValue>,
    part_type: &str,
    kind: PendingAttachmentKind,
    ordinal: usize,
    message_id: i64,
    warnings: &mut WarningSink,
) -> PendingAttachment {
    let reference = attachment_reference(object, part_type);
    let reference_kind = reference
        .map(classify_attachment_reference)
        .unwrap_or(AttachmentReferenceKind::Missing);
    if reference.is_none() {
        push_message_warning(
            warnings,
            message_id,
            WarningCode::AttachmentReferenceMissing,
        );
    }
    let media_type = object
        .get("mime_type")
        .or_else(|| object.get("media_type"))
        .and_then(JsonValue::as_str)
        .and_then(safe_media_type)
        .or_else(|| reference.and_then(media_type_from_data_reference));

    PendingAttachment {
        ordinal,
        kind,
        media_type,
        reference_kind,
        reference_fingerprint: reference.map(|value| digest_bytes(value.as_bytes())),
        import_state: AttachmentImportState::RequiresPolicyValidation,
    }
}

fn attachment_reference<'a>(
    object: &'a serde_json::Map<String, JsonValue>,
    part_type: &str,
) -> Option<&'a str> {
    let primary_key = match part_type {
        "image_url" | "input_image" | "image" => "image_url",
        "input_audio" | "audio" => "input_audio",
        "file" | "input_file" | "document" => "file",
        _ => "url",
    };
    value_or_nested_reference(object.get(primary_key))
        .or_else(|| value_or_nested_reference(object.get("url")))
        .or_else(|| value_or_nested_reference(object.get("file_url")))
        .or_else(|| value_or_nested_reference(object.get("file_id")))
        .or_else(|| value_or_nested_reference(object.get("path")))
        .or_else(|| value_or_nested_reference(object.get("file_path")))
        .or_else(|| value_or_nested_reference(object.get("data")))
}

fn value_or_nested_reference(value: Option<&JsonValue>) -> Option<&str> {
    match value {
        Some(JsonValue::String(value)) if !value.is_empty() => Some(value.as_str()),
        Some(JsonValue::Object(object)) => object
            .get("url")
            .or_else(|| object.get("data"))
            .or_else(|| object.get("path"))
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty()),
        _ => None,
    }
}

fn classify_attachment_reference(reference: &str) -> AttachmentReferenceKind {
    if has_ascii_prefix(reference, "data:") {
        return AttachmentReferenceKind::EmbeddedData;
    }
    if looks_like_absolute_path(reference) || has_ascii_prefix(reference, "file:") {
        return AttachmentReferenceKind::LocalFile;
    }
    if let Ok(url) = Url::parse(reference)
        && matches!(url.scheme(), "http" | "https")
    {
        return AttachmentReferenceKind::RemoteUrl;
    }
    AttachmentReferenceKind::Opaque
}

fn has_ascii_prefix(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn looks_like_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("\\\\")
        || value.starts_with("//")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn media_type_from_data_reference(reference: &str) -> Option<String> {
    if !has_ascii_prefix(reference, "data:") {
        return None;
    }
    let header = reference.get("data:".len()..)?.split([',', ';']).next()?;
    safe_media_type(header)
}

fn safe_media_type(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() > 127 || !value.contains('/') {
        return None;
    }
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'.' | b'-'))
        .then_some(value)
}

fn pick_reasoning(
    role: &str,
    reasoning: Option<&str>,
    reasoning_content: Option<&str>,
    reasoning_details: Option<&str>,
    message_id: i64,
    warnings: &mut WarningSink,
) -> Option<String> {
    if role != "assistant" {
        if [reasoning, reasoning_content, reasoning_details]
            .into_iter()
            .flatten()
            .any(|value| !value.trim().is_empty())
        {
            push_message_warning(warnings, message_id, WarningCode::ReasoningIgnoredForRole);
        }
        return None;
    }

    if let Some(value) = non_empty_trimmed(reasoning) {
        return Some(value.to_owned());
    }
    if let Some(value) = non_empty_trimmed(reasoning_content) {
        return Some(value.to_owned());
    }
    let details = non_empty_trimmed(reasoning_details)?;
    if details.len() > MAX_AUXILIARY_JSON_BYTES {
        push_message_warning(warnings, message_id, WarningCode::ReasoningDetailsTooLarge);
        return None;
    }

    let parsed = match serde_json::from_str::<JsonValue>(details) {
        Ok(parsed) => parsed,
        Err(_) => {
            push_message_warning(
                warnings,
                message_id,
                WarningCode::ReasoningDetailsInvalidJson,
            );
            return None;
        }
    };
    match parsed {
        JsonValue::String(text) => non_empty_trimmed(Some(text.as_str())).map(str::to_owned),
        JsonValue::Array(entries) => {
            let texts = entries
                .iter()
                .filter_map(JsonValue::as_object)
                .filter_map(|entry| {
                    entry
                        .get("text")
                        .or_else(|| entry.get("thinking"))
                        .and_then(JsonValue::as_str)
                        .and_then(|text| non_empty_trimmed(Some(text)))
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>();
            (!texts.is_empty()).then(|| texts.join("\n\n"))
        }
        _ => None,
    }
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_tool_calls(
    raw: Option<&str>,
    message_id: i64,
    warnings: &mut WarningSink,
) -> Vec<ImportedToolCall> {
    let Some(raw) = non_empty_trimmed(raw) else {
        return Vec::new();
    };
    if raw.len() > MAX_AUXILIARY_JSON_BYTES {
        push_message_warning(warnings, message_id, WarningCode::ToolCallsTooLarge);
        return Vec::new();
    }
    let parsed = match serde_json::from_str::<JsonValue>(raw) {
        Ok(parsed) => parsed,
        Err(_) => {
            push_message_warning(warnings, message_id, WarningCode::ToolCallsInvalidJson);
            return Vec::new();
        }
    };
    let JsonValue::Array(entries) = parsed else {
        push_message_warning(warnings, message_id, WarningCode::ToolCallsNotArray);
        return Vec::new();
    };

    let mut tool_calls = Vec::new();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            push_message_warning(warnings, message_id, WarningCode::ToolCallEntryIgnored);
            continue;
        };
        let Some(function) = entry.get("function").and_then(JsonValue::as_object) else {
            push_message_warning(warnings, message_id, WarningCode::ToolCallEntryIgnored);
            continue;
        };
        let Some(name) = function
            .get("name")
            .and_then(JsonValue::as_str)
            .and_then(|name| non_empty_trimmed(Some(name)))
        else {
            push_message_warning(warnings, message_id, WarningCode::ToolCallEntryIgnored);
            continue;
        };
        let call_id = entry
            .get("call_id")
            .or_else(|| entry.get("id"))
            .and_then(JsonValue::as_str)
            .and_then(|value| non_empty_trimmed(Some(value)))
            .map(str::to_owned);
        let (arguments, arguments_format) = match function.get("arguments") {
            Some(JsonValue::String(raw_arguments)) => {
                match serde_json::from_str::<JsonValue>(raw_arguments) {
                    Ok(arguments) => (arguments, ToolArgumentsFormat::Json),
                    Err(_) => {
                        push_message_warning(
                            warnings,
                            message_id,
                            WarningCode::ToolCallArgumentsInvalidJson,
                        );
                        (
                            JsonValue::String(raw_arguments.clone()),
                            ToolArgumentsFormat::Text,
                        )
                    }
                }
            }
            Some(arguments) => (arguments.clone(), ToolArgumentsFormat::Json),
            None => (JsonValue::Null, ToolArgumentsFormat::Missing),
        };
        tool_calls.push(ImportedToolCall {
            call_id,
            name: name.to_owned(),
            arguments,
            arguments_format,
        });
    }
    tool_calls
}

fn push_message_warning(warnings: &mut WarningSink, message_id: i64, code: WarningCode) {
    warnings.push(ImportWarning {
        code,
        table: SourceTable::Messages,
        record_number: Some(message_id),
    });
}

fn record_provenance(
    table: SourceTable,
    source_key: &[u8],
    row: &JsonValue,
) -> Result<RecordProvenance, HermesV21Error> {
    Ok(RecordProvenance {
        table,
        source_key_digest: digest_bytes(source_key),
        source_row_digest: digest_json(row)?,
    })
}

fn logical_fingerprint(
    sessions: &[HermesV21Session],
    messages: &[HermesV21Message],
    model_usage: &[HermesV21ModelUsage],
) -> Result<String, HermesV21Error> {
    let bytes = serde_json::to_vec(&(
        HERMES_AGENT_COMMIT,
        HERMES_V21_SCHEMA_VERSION,
        sessions,
        messages,
        model_usage,
    ))
    .map_err(|_| HermesV21Error::FingerprintFailed)?;
    Ok(digest_bytes(&bytes))
}

fn digest_json(value: &JsonValue) -> Result<String, HermesV21Error> {
    serde_json::to_vec(value)
        .map(|bytes| digest_bytes(&bytes))
        .map_err(|_| HermesV21Error::FingerprintFailed)
}

fn digest_bytes(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}
