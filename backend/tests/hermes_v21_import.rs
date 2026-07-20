use std::fs;

use rusqlite::Connection;
use synthchat_hermes_backend::{
    SessionService,
    compat::hermes_v21::read_snapshot,
    sessions::{
        HermesImportConflictCode, HermesImportDisposition, HermesImportError,
        HermesV21ImportRequest, ListMessages, ListSessions, ToolCallStatus,
    },
};
use tempfile::TempDir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const V21_FIXTURE: &str = include_str!("fixtures/hermes_v21.sql");

struct Fixture {
    home: TempDir,
    service: SessionService,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let source_path = home.path().join("state.db");
        Connection::open(&source_path)
            .unwrap()
            .execute_batch(V21_FIXTURE)
            .unwrap();
        let service = SessionService::new(home.path(), TOKEN);
        Self { home, service }
    }

    fn snapshot(&self) -> synthchat_hermes_backend::compat::hermes_v21::HermesV21Snapshot {
        read_snapshot(self.home.path().join("state.db")).unwrap()
    }

    fn sessions(&self) -> Vec<synthchat_hermes_backend::sessions::Session> {
        self.service
            .list_sessions(&ListSessions {
                profile_id: "default".to_owned(),
                query: None,
                archived: false,
                cursor: None,
                limit: 100,
            })
            .unwrap()
            .items
    }
}

#[test]
fn preview_and_atomic_import_preserve_history_context_and_provenance() {
    let fixture = Fixture::new();
    let source_path = fixture.home.path().join("state.db");
    let source_before = fs::read(&source_path).unwrap();
    let snapshot = fixture.snapshot();
    let preview = fixture
        .service
        .preview_from_snapshot("default", &snapshot)
        .unwrap();
    assert_eq!(preview.state, "ready");
    assert_eq!(preview.session_count, Some(1));
    assert_eq!(preview.message_count, Some(5));
    assert_eq!(preview.model_usage_row_count, Some(1));
    assert_eq!(preview.attachment_count, Some(3));
    assert_eq!(preview.rewound_message_count, Some(1));

    let denied = HermesV21ImportRequest {
        expected_snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: false,
    };
    assert_eq!(
        fixture.service.import_hermes_v21_snapshot(
            "default",
            &snapshot,
            &denied,
            "import-denied-0001",
        ),
        Err(HermesImportError::AttachmentsRequirePolicy)
    );
    assert!(fixture.sessions().is_empty());

    let request = HermesV21ImportRequest {
        expected_snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: true,
    };
    let imported = fixture
        .service
        .import_hermes_v21_snapshot("default", &snapshot, &request, "import-fixture-0001")
        .unwrap();
    assert_eq!(imported.disposition, HermesImportDisposition::Imported);
    assert_eq!(imported.imported_session_count, 1);
    assert_eq!(imported.imported_message_count, 5);
    assert_eq!(imported.imported_model_usage_row_count, 1);
    assert_eq!(imported.omitted_attachment_count, 3);

    let sessions = fixture.sessions();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].id.starts_with("session_hv21_"));
    assert_eq!(sessions[0].title, "Synthetic fixture");
    assert_eq!(sessions[0].message_count, 5);
    assert_eq!(sessions[0].source, "hermes-agent:v21");
    let messages = fixture
        .service
        .list_messages(
            &sessions[0].id,
            &ListMessages {
                cursor: None,
                limit: 100,
            },
        )
        .unwrap()
        .items;
    assert_eq!(messages.len(), 5);
    assert!(messages.iter().all(|message| message.usage.is_none()));
    let multimodal = messages
        .iter()
        .find(|message| {
            message.parts.iter().any(|part| {
                matches!(part, synthchat_hermes_backend::sessions::MessagePart::Text { text } if text == "Multimodal text")
            })
        })
        .unwrap();
    assert_eq!(multimodal.tool_calls[0].status, ToolCallStatus::Unknown);

    let target = Connection::open(fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let compacted_context: i64 = target
        .query_row(
            "SELECT context_eligible FROM messages WHERE searchable_text = 'Multimodal text'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(compacted_context, 0);
    let usage_rows: i64 = target
        .query_row(
            "SELECT COUNT(*) FROM hermes_import_model_usage",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usage_rows, 1);
    assert_eq!(fs::read(source_path).unwrap(), source_before);
}

#[test]
fn idempotency_and_unchanged_snapshots_survive_restart() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let request = HermesV21ImportRequest {
        expected_snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: true,
    };
    let first = fixture
        .service
        .import_hermes_v21_snapshot("default", &snapshot, &request, "import-restart-0001")
        .unwrap();
    let restarted = SessionService::new(fixture.home.path(), "new-launch-token");
    let replay = restarted
        .lookup_hermes_v21_replay("default", "import-restart-0001", &request)
        .unwrap()
        .unwrap();
    assert_eq!(replay.import_id, first.import_id);
    assert_eq!(replay.disposition, HermesImportDisposition::Replayed);

    let unchanged = restarted
        .import_hermes_v21_snapshot("default", &snapshot, &request, "import-restart-0002")
        .unwrap();
    assert_eq!(unchanged.import_id, first.import_id);
    assert_eq!(unchanged.disposition, HermesImportDisposition::Unchanged);
    assert_eq!(unchanged.imported_session_count, 0);
    assert_eq!(
        restarted
            .list_sessions(&ListSessions {
                profile_id: "default".to_owned(),
                query: None,
                archived: false,
                cursor: None,
                limit: 100,
            })
            .unwrap()
            .items
            .len(),
        1
    );
}

#[test]
fn source_conflict_rolls_back_unrelated_new_sessions() {
    let fixture = Fixture::new();
    let first_snapshot = fixture.snapshot();
    let first_request = HermesV21ImportRequest {
        expected_snapshot_fingerprint: first_snapshot.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: true,
    };
    fixture
        .service
        .import_hermes_v21_snapshot(
            "default",
            &first_snapshot,
            &first_request,
            "import-conflict-0001",
        )
        .unwrap();

    let source = Connection::open(fixture.home.path().join("state.db")).unwrap();
    source
        .execute(
            "UPDATE sessions SET title = 'Changed upstream' WHERE id = 'synthetic-session-1'",
            [],
        )
        .unwrap();
    source
        .execute_batch(
            "INSERT INTO sessions(\
           id, source, model, started_at, message_count, title, archived\
         ) VALUES('new-unrelated', 'cli', 'synthetic/model', 2000, 1, 'New unrelated', 0);\
         INSERT INTO messages(session_id, role, content, timestamp, active, compacted)\
         VALUES('new-unrelated', 'user', 'Must roll back', 2001, 1, 0);",
        )
        .unwrap();
    drop(source);
    let changed = fixture.snapshot();
    let changed_request = HermesV21ImportRequest {
        expected_snapshot_fingerprint: changed.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: true,
    };
    let error = fixture
        .service
        .import_hermes_v21_snapshot(
            "default",
            &changed,
            &changed_request,
            "import-conflict-0002",
        )
        .unwrap_err();
    let HermesImportError::Conflict(report) = error else {
        panic!("expected a bounded import conflict");
    };
    assert!(
        report
            .conflicts
            .iter()
            .any(|conflict| { conflict.code == HermesImportConflictCode::SourceChanged })
    );
    assert_eq!(
        fixture.sessions().len(),
        1,
        "the unrelated new Session must roll back"
    );
    let target = Connection::open(fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let batches: i64 = target
        .query_row("SELECT COUNT(*) FROM hermes_import_batches", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(batches, 1);
}

#[test]
fn deleting_an_imported_target_creates_a_tombstone_conflict_without_resurrection() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let request = HermesV21ImportRequest {
        expected_snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: true,
    };
    fixture
        .service
        .import_hermes_v21_snapshot("default", &snapshot, &request, "import-delete-0001")
        .unwrap();
    let session = fixture.sessions().remove(0);
    fixture
        .service
        .delete_session(&session.id, Some(&format!("\"{}\"", session.revision)))
        .unwrap();
    let error = fixture
        .service
        .import_hermes_v21_snapshot("default", &snapshot, &request, "import-delete-0002")
        .unwrap_err();
    let HermesImportError::Conflict(report) = error else {
        panic!("expected a target tombstone conflict");
    };
    assert!(
        report
            .conflicts
            .iter()
            .any(|conflict| { conflict.code == HermesImportConflictCode::TargetDeleted })
    );
    assert!(fixture.sessions().is_empty());
}

#[test]
fn a_mid_transaction_database_failure_leaves_no_partial_import() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let request = HermesV21ImportRequest {
        expected_snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
        allow_attachment_omission: true,
    };
    let target_path = fixture.home.path().join(".synthchat/sessions-v1.db");
    Connection::open(&target_path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_hermes_message BEFORE INSERT ON messages \
             WHEN new.id LIKE 'message_hv21_%' BEGIN \
               SELECT RAISE(ABORT, 'synthetic import failure'); \
             END;",
        )
        .unwrap();

    assert_eq!(
        fixture.service.import_hermes_v21_snapshot(
            "default",
            &snapshot,
            &request,
            "import-rollback-0001",
        ),
        Err(HermesImportError::StorageUnavailable)
    );
    let target = Connection::open(target_path).unwrap();
    for table in [
        "sessions",
        "messages",
        "hermes_import_batches",
        "hermes_import_session_map",
        "hermes_import_message_map",
        "hermes_import_model_usage",
    ] {
        let count: i64 = target
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} must roll back with the failed import");
    }
}
