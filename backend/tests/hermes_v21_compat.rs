#[path = "../src/compat/mod.rs"]
mod compat;

use std::fs;
use std::path::{Path, PathBuf};

use compat::hermes_v21::{
    AttachmentReferenceKind, HERMES_AGENT_COMMIT, HERMES_AGENT_REPOSITORY,
    HERMES_V21_SCHEMA_VERSION, HermesV21Error, PendingAttachmentKind, ToolArgumentsFormat,
    WarningCode, read_snapshot,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const V21_FIXTURE: &str = include_str!("fixtures/hermes_v21.sql");
const UPSTREAM_LOCK: &str = include_str!("../../docs/upstream-lock.json");

fn synthetic_database() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("create synthetic fixture directory");
    let path = directory.path().join("state.db");
    let connection = Connection::open(&path).expect("create synthetic state database");
    connection
        .execute_batch(V21_FIXTURE)
        .expect("install synthetic v21 fixture");
    connection.close().expect("close synthetic fixture writer");
    (directory, path)
}

#[test]
fn adapter_constants_match_the_checked_in_upstream_lock() {
    let lock: serde_json::Value = serde_json::from_str(UPSTREAM_LOCK).expect("parse upstream lock");
    let agent = &lock["sources"]["hermesAgent"];
    assert_eq!(agent["repository"], HERMES_AGENT_REPOSITORY);
    assert_eq!(agent["commit"], HERMES_AGENT_COMMIT);
    assert_eq!(
        agent["evidence"][0]["sha256"],
        "A4F6C6FBC11AFD2228979AABFC24368F370DFE73D212A0083D95E9CD25AEA2A8"
    );
}

fn message(
    snapshot: &compat::hermes_v21::HermesV21Snapshot,
    upstream_id: i64,
) -> &compat::hermes_v21::HermesV21Message {
    snapshot
        .messages
        .iter()
        .find(|message| message.upstream_id == upstream_id)
        .expect("fixture message exists")
}

#[test]
fn reads_v21_snapshot_in_stable_order_and_filters_rewound_rows() {
    let (_directory, path) = synthetic_database();
    let before = fs::read(&path).expect("read source bytes before import");

    let snapshot = read_snapshot(&path).expect("read fixed v21 schema");

    assert_eq!(
        snapshot.provenance.schema_version,
        HERMES_V21_SCHEMA_VERSION
    );
    assert_eq!(snapshot.provenance.upstream_commit, HERMES_AGENT_COMMIT);
    assert_eq!(snapshot.provenance.logical_fingerprint.len(), 64);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].upstream_id, "synthetic-session-1");
    assert_eq!(snapshot.sessions[0].aggregate_usage.input_tokens, 120);

    let ids = snapshot
        .messages
        .iter()
        .map(|message| message.upstream_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![11, 12, 10, 14, 15]);
    assert!(!ids.contains(&13));
    assert!(message(&snapshot, 12).compacted);
    assert!(!message(&snapshot, 12).active);
    assert!(message(&snapshot, 10).active);
    assert_eq!(snapshot.statistics.rewound_message_count, 1);
    assert!(snapshot.warnings.iter().any(|warning| {
        warning.code == WarningCode::ActiveNullTreatedAsActive && warning.record_number == Some(10)
    }));

    assert_eq!(snapshot.model_usage.len(), 1);
    assert_eq!(snapshot.model_usage[0].input_tokens, 120);
    assert!(snapshot.model_usage[0].billing_base_url_present);
    assert_eq!(snapshot.model_usage[0].route_fingerprint.len(), 64);

    let after = fs::read(&path).expect("read source bytes after import");
    assert_eq!(
        before, after,
        "the read-only adapter must not mutate state.db"
    );
    assert_eq!(
        snapshot,
        read_snapshot(&path).expect("repeat same snapshot")
    );
}

#[test]
fn follows_reasoning_priority_and_tolerates_tool_call_damage() {
    let (_directory, path) = synthetic_database();
    let snapshot = read_snapshot(path).expect("read fixed v21 schema");

    assert_eq!(
        message(&snapshot, 11).reasoning.as_deref(),
        Some("Direct reasoning")
    );
    assert_eq!(
        message(&snapshot, 12).reasoning.as_deref(),
        Some("Legacy reasoning")
    );
    assert_eq!(
        message(&snapshot, 10).reasoning.as_deref(),
        Some("First detail\n\nSecond detail")
    );
    assert_eq!(message(&snapshot, 14).reasoning, None);
    assert_eq!(message(&snapshot, 15).reasoning, None);

    let valid_calls = &message(&snapshot, 12).tool_calls;
    assert_eq!(valid_calls.len(), 1);
    assert_eq!(valid_calls[0].call_id.as_deref(), Some("call-1"));
    assert_eq!(valid_calls[0].name, "terminal");
    assert_eq!(valid_calls[0].arguments_format, ToolArgumentsFormat::Json);
    assert_eq!(valid_calls[0].arguments["command"], "echo synthetic");

    assert!(message(&snapshot, 14).tool_calls.is_empty());
    let text_arguments = &message(&snapshot, 15).tool_calls[0];
    assert_eq!(text_arguments.arguments_format, ToolArgumentsFormat::Text);
    assert_eq!(text_arguments.arguments, "not-json");

    let warning_codes = snapshot
        .warnings
        .iter()
        .map(|warning| warning.code)
        .collect::<Vec<_>>();
    assert!(warning_codes.contains(&WarningCode::ToolCallEntryIgnored));
    assert!(warning_codes.contains(&WarningCode::ToolCallsInvalidJson));
    assert!(warning_codes.contains(&WarningCode::ToolCallArgumentsInvalidJson));
    assert!(warning_codes.contains(&WarningCode::ReasoningDetailsInvalidJson));
    assert!(warning_codes.contains(&WarningCode::ReasoningIgnoredForRole));
}

#[test]
fn extracts_text_but_never_returns_attachment_references_or_database_paths() {
    let (_directory, path) = synthetic_database();
    let snapshot = read_snapshot(&path).expect("read fixed v21 schema");
    let multimodal = message(&snapshot, 12);

    assert_eq!(multimodal.content.text.as_deref(), Some("Multimodal text"));
    let attachments = &multimodal.content.pending_attachments;
    assert_eq!(attachments.len(), 3);
    assert!(
        attachments
            .iter()
            .all(|attachment| attachment.kind == PendingAttachmentKind::Image)
    );
    assert_eq!(
        attachments
            .iter()
            .map(|attachment| attachment.reference_kind)
            .collect::<Vec<_>>(),
        vec![
            AttachmentReferenceKind::EmbeddedData,
            AttachmentReferenceKind::LocalFile,
            AttachmentReferenceKind::RemoteUrl,
        ]
    );
    assert_eq!(attachments[0].media_type.as_deref(), Some("image/png"));
    assert!(attachments.iter().all(|attachment| {
        attachment
            .reference_fingerprint
            .as_ref()
            .is_some_and(|fingerprint| fingerprint.len() == 64)
    }));

    let serialized = serde_json::to_string(&snapshot).expect("serialize neutral DTO");
    assert!(!serialized.contains("data:image"));
    assert!(!serialized.contains("synthetic\\\\fixture.png"));
    assert!(!serialized.contains("assets.example.test"));
    assert!(!serialized.contains("private-route.example.test"));
    assert!(!serialized.contains(path.to_string_lossy().as_ref()));
    assert!(!serialized.contains("json:{bad"));
}

#[test]
fn rejects_unknown_versions_and_missing_required_columns() {
    let (_directory, path) = synthetic_database();
    let connection = Connection::open(&path).expect("open fixture writer");
    connection
        .execute("UPDATE schema_version SET version = 20", [])
        .expect("downgrade fixture version");
    connection.close().expect("close fixture writer");

    assert_eq!(
        read_snapshot(&path),
        Err(HermesV21Error::UnsupportedSchemaVersion { found: 20 })
    );

    let directory = tempfile::tempdir().expect("create missing-column fixture directory");
    let missing_path = directory.path().join("missing.db");
    let connection = Connection::open(&missing_path).expect("create missing-column fixture");
    connection
        .execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);\
             INSERT INTO schema_version VALUES (21);\
             CREATE TABLE sessions (\
               id TEXT, source TEXT, model TEXT, parent_session_id TEXT,\
               started_at REAL, ended_at REAL, message_count INTEGER,\
               tool_call_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,\
               cache_read_tokens INTEGER, cache_write_tokens INTEGER,\
               reasoning_tokens INTEGER, estimated_cost_usd REAL, actual_cost_usd REAL,\
               api_call_count INTEGER, archived INTEGER\
             );",
        )
        .expect("install missing-column fixture");
    connection.close().expect("close missing-column fixture");

    assert_eq!(
        read_snapshot(missing_path),
        Err(HermesV21Error::MissingColumn {
            table: "sessions",
            column: "title",
        })
    );
}

#[test]
fn warnings_are_bounded_and_errors_do_not_echo_source_paths() {
    let (_directory, path) = synthetic_database();
    let connection = Connection::open(&path).expect("open fixture writer");
    for id in 100..500 {
        connection
            .execute(
                "INSERT INTO messages (id, session_id, role, content, tool_calls, timestamp, active, compacted) \
                 VALUES (?, 'synthetic-session-1', 'assistant', ?, ?, ?, 1, 0)",
                params![id, format!("\0json:{{bad-{id}"), "{bad", id as f64],
            )
            .expect("insert synthetic damaged row");
    }
    connection.close().expect("close fixture writer");

    let snapshot = read_snapshot(&path).expect("damaged rows remain importable");
    assert_eq!(snapshot.warnings.len(), 256);
    assert!(snapshot.warnings_dropped > 0);

    let missing = path.with_file_name("source-path-must-not-leak.db");
    let error = read_snapshot(&missing).expect_err("missing source must fail");
    assert_eq!(error, HermesV21Error::OpenFailed);
    let rendered = error.to_string();
    assert!(!rendered.contains(missing.to_string_lossy().as_ref()));
    assert!(!rendered.contains("source-path-must-not-leak"));
}

#[test]
fn reads_a_committed_wal_snapshot_without_requiring_fts_tables() {
    let directory = tempfile::tempdir().expect("create WAL fixture directory");
    let path = directory.path().join("state.db");
    let writer = Connection::open(&path).expect("create WAL fixture");
    writer
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL");
    writer
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("disable automatic checkpoint");
    writer
        .execute_batch(V21_FIXTURE)
        .expect("install fixture into WAL database");
    writer
        .execute(
            "INSERT INTO messages (id, session_id, role, content, timestamp, active, compacted) \
             VALUES (99, 'synthetic-session-1', 'user', 'Committed in WAL', 9.0, 1, 0)",
            [],
        )
        .expect("commit a WAL-only message");

    let snapshot = read_snapshot(&path).expect("read consistent WAL snapshot");
    assert_eq!(
        message(&snapshot, 99).content.text.as_deref(),
        Some("Committed in WAL")
    );
    assert!(
        writer
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE name = 'messages_fts'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .is_err(),
        "the fixture intentionally has no FTS table"
    );
}

#[allow(dead_code)]
fn _assert_path_is_not_part_of_the_public_contract(_: &Path) {}
