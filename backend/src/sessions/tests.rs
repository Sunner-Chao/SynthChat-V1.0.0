use std::sync::{Arc, Barrier};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::{
    profiles::Versioned,
    runs::{
        CancelDisposition, ChatInput, CreateRun, PendingAction, RunError, RunProblem, RunStatus,
    },
};

use super::{
    ClarificationContinuationBinding, ClarificationError, ClarificationRequest,
    ClarificationResolutionDisposition, ClarificationResolvedBy, ClarificationState, CommitMessage,
    CompleteRunPlan, CreateSession, ListMessages, ListSessions, MessagePart, MessageRole,
    PatchField, ProviderContextMessage, ProviderTurnFinish, ProviderTurnPlan, RawToolCallPlan,
    RuntimeLeaseState, SESSION_SCHEMA_VERSION, SearchField, SearchMode, Session, SessionError,
    SessionPatch, SessionService, ToolApprovalDecision, ToolApprovalError,
    ToolApprovalExecutionBinding, ToolApprovalRequest, ToolApprovalResolutionDisposition,
    ToolApprovalResolvedBy, ToolApprovalState, ToolInvocationOrigin, ToolInvocationStatus, Usage,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const APPROVAL_ARGUMENTS_JSON: &str = r#"{"command":"cargo test"}"#;
const CLARIFICATION_ARGUMENTS_JSON: &str =
    r#"{"question":"Choose a target","choices":["staging","production"]}"#;

struct Fixture {
    home: TempDir,
    service: SessionService,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let service = SessionService::new(home.path(), TOKEN);
        Self { home, service }
    }

    fn create(&self, title: &str, key: &str) -> Versioned<Session> {
        self.service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some(title.to_owned()),
                },
                key,
            )
            .unwrap()
    }
}

fn text_message(text: &str) -> CommitMessage {
    CommitMessage {
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            text: text.to_owned(),
        }],
        reasoning: None,
        tool_calls: Vec::new(),
        usage: None,
        model: Some("test/model".to_owned()),
    }
}

fn create_started_run(
    service: &SessionService,
    session_id: &str,
    key: &str,
    message_id: &str,
) -> String {
    let accepted = service
        .create_run(
            session_id,
            &CreateRun {
                client_request_id: format!("client-{key}"),
                message: ChatInput {
                    text: "invoke tools".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: None,
            },
            key,
            "test/model",
        )
        .unwrap();
    service
        .begin_assistant_message(&accepted.run.id, message_id)
        .unwrap();
    accepted.run.id
}

#[test]
fn active_run_discovery_is_stable_atomic_and_owner_filtered() {
    let fixture = Fixture::new();
    let first_session = fixture.create("first active", "active-discovery-session-1");
    let second_session = fixture.create("second active", "active-discovery-session-2");
    let other_session = fixture
        .service
        .create_session(
            &CreateSession {
                profile_id: "other".to_owned(),
                title: Some("other profile".to_owned()),
            },
            "active-discovery-session-3",
        )
        .unwrap();
    let create = |session_id: &str, key: &str, text: &str| {
        fixture
            .service
            .create_run(
                session_id,
                &CreateRun {
                    client_request_id: format!("client-{key}"),
                    message: ChatInput {
                        text: text.to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                key,
                "test/model",
            )
            .unwrap()
    };
    let first = create(
        &first_session.value.id,
        "active-discovery-run-1",
        "first message",
    );
    let second = create(
        &second_session.value.id,
        "active-discovery-run-2",
        "second message",
    );
    let _other = create(
        &other_session.value.id,
        "active-discovery-run-3",
        "other message",
    );

    let discovered = fixture.service.list_active_runs("default", None).unwrap();
    assert_eq!(discovered.items.len(), 2);
    assert_eq!(discovered.items[0].run.id, first.run.id);
    assert_eq!(discovered.items[1].run.id, second.run.id);
    assert_eq!(discovered.items[0].user_message, first.user_message);
    assert_eq!(discovered.items[0].session_revision, first.session_revision);
    assert!(discovered.items.iter().all(|item| {
        item.run.profile_id == "default"
            && item.user_message.role == MessageRole::User
            && item.user_message.session_id == item.run.session_id
            && item.queue_item_id.is_none()
    }));

    let filtered = fixture
        .service
        .list_active_runs("default", Some(&second_session.value.id))
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].run.id, second.run.id);
    assert!(
        fixture
            .service
            .list_active_runs("default", Some("session_00000000000000000000000000000000"))
            .unwrap()
            .items
            .is_empty()
    );

    fixture
        .service
        .cancel_run_terminal(&first.run.id, "test cleanup")
        .unwrap();
    let after_terminal = fixture.service.list_active_runs("default", None).unwrap();
    assert_eq!(after_terminal.items.len(), 1);
    assert_eq!(after_terminal.items[0].run.id, second.run.id);
    assert!(matches!(
        fixture.service.list_active_runs("Default", None),
        Err(RunError::InvalidRequest)
    ));
    assert!(matches!(
        fixture
            .service
            .list_active_runs("default", Some("not-a-session")),
        Err(RunError::InvalidRequest)
    ));
}

#[test]
fn persistent_run_queue_is_fifo_idempotent_and_preserves_logical_context_order() {
    let fixture = Fixture::new();
    let session = fixture.create("queued runs", "queue-session-key");
    let request = |id: &str, text: &str| CreateRun {
        client_request_id: id.to_owned(),
        message: ChatInput {
            text: text.to_owned(),
            file_ids: Vec::new(),
        },
        model_override: None,
        reasoning_effort: None,
        workspace_id: None,
    };
    let first_request = request("queue-client-1", "first user");
    let second_request = request("queue-client-2", "second user");
    let third_request = request("queue-client-3", "third user");
    let first = fixture
        .service
        .create_run(
            &session.value.id,
            &first_request,
            "queue-run-key-1",
            "test/model",
        )
        .unwrap();
    let second = fixture
        .service
        .create_run(
            &session.value.id,
            &second_request,
            "queue-run-key-2",
            "test/model",
        )
        .unwrap();
    let third = fixture
        .service
        .create_run(
            &session.value.id,
            &third_request,
            "queue-run-key-3",
            "test/model",
        )
        .unwrap();
    assert_eq!(first.run.status, RunStatus::Running);
    assert_eq!(second.run.status, RunStatus::Queued);
    assert_eq!(third.run.status, RunStatus::Queued);
    assert!(second.queue_item_id.is_some());
    assert!(third.queue_item_id.is_some());
    let replay = fixture
        .service
        .create_run(
            &session.value.id,
            &second_request,
            "queue-run-key-2",
            "test/model",
        )
        .unwrap();
    assert_eq!(replay.run.id, second.run.id);
    assert_eq!(replay.queue_item_id, second.queue_item_id);

    let first_message_id = "message_queue_first_assistant";
    fixture
        .service
        .begin_assistant_message(&first.run.id, first_message_id)
        .unwrap();
    fixture
        .service
        .complete_run(
            &first.run.id,
            CompleteRunPlan {
                message_id: first_message_id.to_owned(),
                text: "first assistant".to_owned(),
                reasoning: None,
                tool_calls: Vec::new(),
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: None,
                },
                model_label: "test/model".to_owned(),
            },
        )
        .unwrap();
    let claimed_second = fixture
        .service
        .claim_next_queued_run(&session.value.id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed_second.run.id, second.run.id);
    assert_eq!(claimed_second.request, second_request);
    assert_eq!(
        fixture
            .service
            .provider_continuation_context(&claimed_second.run.id)
            .unwrap(),
        vec![
            ProviderContextMessage::User {
                content: "first user".to_owned(),
            },
            ProviderContextMessage::Assistant {
                content: Some("first assistant".to_owned()),
                tool_calls: Vec::new(),
            },
            ProviderContextMessage::User {
                content: "second user".to_owned(),
            },
        ]
    );
    fixture
        .service
        .cancel_run_terminal(&claimed_second.run.id, "advance test")
        .unwrap();
    let claimed_third = fixture
        .service
        .claim_next_queued_run(&session.value.id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed_third.run.id, third.run.id);
    assert_eq!(claimed_third.request, third_request);
}

#[test]
fn queued_cancel_is_durable_and_restart_recovery_preserves_the_next_item() {
    let home = tempfile::tempdir().unwrap();
    let first_service = SessionService::new(home.path(), TOKEN);
    let session = first_service
        .create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("queue recovery".to_owned()),
            },
            "queue-recovery-session",
        )
        .unwrap();
    let create = |service: &SessionService, key: &str, text: &str| {
        service
            .create_run(
                &session.value.id,
                &CreateRun {
                    client_request_id: format!("client-{key}"),
                    message: ChatInput {
                        text: text.to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                key,
                "test/model",
            )
            .unwrap()
    };
    let running = create(&first_service, "queue-recovery-run-1", "running");
    let cancelled = create(&first_service, "queue-recovery-run-2", "cancel queued");
    let retained = create(&first_service, "queue-recovery-run-3", "retain queued");
    let (cancelled, disposition) = first_service.request_run_cancel(&cancelled.run.id).unwrap();
    assert_eq!(disposition, CancelDisposition::CancelledQueued);
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(
        run_event_names(&first_service, &cancelled.id),
        ["run.queued", "run.cancelled"]
    );

    let restarted = SessionService::new(home.path(), TOKEN);
    assert_eq!(
        restarted.recover_interrupted_runs().unwrap(),
        vec![running.run.id]
    );
    assert_eq!(
        restarted.get_run(&retained.run.id).unwrap().status,
        RunStatus::Queued
    );
    let claim = restarted
        .claim_next_queued_run(&session.value.id)
        .unwrap()
        .unwrap();
    assert_eq!(claim.run.id, retained.run.id);
    assert_eq!(
        run_event_names(&restarted, &claim.run.id),
        ["run.queued", "run.started"]
    );
}

#[test]
fn runtime_lease_ttl_expiry_allows_takeover_and_epoch_fences_the_displaced_owner() {
    let home = tempfile::tempdir().unwrap();
    let first = SessionService::new(home.path(), TOKEN);
    let session = first
        .create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("runtime fence".to_owned()),
            },
            "runtime-fence-session",
        )
        .unwrap();
    let first_lease = first
        .acquire_runtime_lease("runtime_11111111111111111111111111111111")
        .unwrap();
    let run = first
        .create_run(
            &session.value.id,
            &CreateRun {
                client_request_id: "runtime-fence-client".to_owned(),
                message: ChatInput {
                    text: "fence me".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: None,
            },
            "runtime-fence-run",
            "test/model",
        )
        .unwrap();
    let replacement = SessionService::new(home.path(), TOKEN);
    assert!(matches!(
        replacement.acquire_runtime_lease("runtime_22222222222222222222222222222222"),
        Err(RunError::EngineUnavailable)
    ));
    assert!(matches!(
        replacement.create_run(
            &session.value.id,
            &CreateRun {
                client_request_id: "runtime-fence-contender-client".to_owned(),
                message: ChatInput {
                    text: "failed contender must stay fenced".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: None,
            },
            "runtime-fence-contender-run",
            "test/model",
        ),
        Err(RunError::EngineUnavailable)
    ));
    first
        .begin_assistant_message(&run.run.id, "message_runtime_fence")
        .unwrap();
    let connection =
        rusqlite::Connection::open(home.path().join(".synthchat/sessions-v1.db")).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE runtime_leases SET expires_at_unix_ms = 0 \
                 WHERE lease_name = 'run-runtime'",
                [],
            )
            .unwrap(),
        1
    );
    let replacement_lease = replacement
        .acquire_runtime_lease("runtime_22222222222222222222222222222222")
        .unwrap();
    assert!(replacement_lease.epoch > first_lease.epoch);
    assert!(matches!(
        first.renew_runtime_lease(&first_lease),
        Err(RunError::EngineUnavailable)
    ));
    assert!(matches!(
        first.create_run(
            &session.value.id,
            &CreateRun {
                client_request_id: "runtime-fence-stale-client".to_owned(),
                message: ChatInput {
                    text: "stale owner must not enqueue".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: None,
            },
            "runtime-fence-stale-run",
            "test/model",
        ),
        Err(RunError::EngineUnavailable)
    ));
    assert!(matches!(
        first.cancel_run_terminal(&run.run.id, "stale owner"),
        Err(RunError::EngineUnavailable)
    ));
    assert_eq!(
        replacement.recover_interrupted_runs().unwrap(),
        vec![run.run.id]
    );
}

#[test]
fn released_runtime_lease_fences_late_writes_until_a_new_epoch_is_acquired() {
    let home = tempfile::tempdir().unwrap();
    let service = SessionService::new(home.path(), TOKEN);
    let session = service
        .create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("released runtime fence".to_owned()),
            },
            "released-runtime-fence-session",
        )
        .unwrap();
    let first = service
        .acquire_runtime_lease("runtime_11111111111111111111111111111111")
        .unwrap();
    service.release_runtime_lease(&first);

    assert!(matches!(
        service.create_run(
            &session.value.id,
            &CreateRun {
                client_request_id: "released-runtime-stale-client".to_owned(),
                message: ChatInput {
                    text: "late write must stay fenced".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: None,
            },
            "released-runtime-stale-run",
            "test/model",
        ),
        Err(RunError::EngineUnavailable)
    ));

    let second = service
        .acquire_runtime_lease("runtime_22222222222222222222222222222222")
        .unwrap();
    assert!(second.epoch > first.epoch);
    assert!(matches!(
        service.renew_runtime_lease(&first),
        Err(RunError::EngineUnavailable)
    ));
    service.release_runtime_lease(&first);
    service.renew_runtime_lease(&second).unwrap();
    let accepted = service
        .create_run(
            &session.value.id,
            &CreateRun {
                client_request_id: "released-runtime-current-client".to_owned(),
                message: ChatInput {
                    text: "new epoch may write".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: None,
            },
            "released-runtime-current-run",
            "test/model",
        )
        .unwrap();
    service.release_runtime_lease(&second);
    assert!(matches!(
        service.cancel_run_terminal(&accepted.run.id, "late shutdown write"),
        Err(RunError::EngineUnavailable)
    ));
}

#[test]
fn runtime_lease_release_waits_for_in_flight_writes_before_fencing() {
    let home = tempfile::tempdir().unwrap();
    let service = SessionService::new(home.path(), TOKEN);
    let lease = service
        .acquire_runtime_lease("runtime_11111111111111111111111111111111")
        .unwrap();
    let ready = service.ready().unwrap().clone();
    let write_guard = ready.write_lock.lock().unwrap();
    let releasing_service = service.clone();
    let releasing_lease = lease.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let release = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        releasing_service.release_runtime_lease(&releasing_lease);
        finished_tx.send(()).unwrap();
    });

    started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    assert!(
        finished_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "release returned while a write transaction could still commit"
    );
    drop(write_guard);
    finished_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    release.join().unwrap();
    assert_eq!(service.runtime_lease_state(), RuntimeLeaseState::Fenced);
}

fn tool_turn(message_id: &str, calls: Vec<RawToolCallPlan>) -> ProviderTurnPlan {
    ProviderTurnPlan {
        turn_index: 1,
        assistant_message_id: message_id.to_owned(),
        content: None,
        reasoning: Some("selecting tools".to_owned()),
        finish: ProviderTurnFinish::ToolCalls,
        usage: Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            total_tokens: 12,
            cost: None,
        },
        tool_calls: calls,
    }
}

fn approval_expiry_after(milliseconds: i64) -> String {
    use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

    (OffsetDateTime::now_utc() + Duration::milliseconds(milliseconds))
        .format(&Rfc3339)
        .unwrap()
}

fn approval_execution_binding(
    service: &SessionService,
    run_id: &str,
    request: &ToolApprovalRequest,
) -> ToolApprovalExecutionBinding {
    let run = service.get_run(run_id).unwrap();
    let approval = service
        .load_tool_approval(run_id, &request.approval_id)
        .unwrap();
    ToolApprovalExecutionBinding {
        run_id: run.id,
        profile_id: run.profile_id,
        session_id: run.session_id,
        workspace_id: None,
        call_id: request.call_id.clone(),
        tool_name: request.tool_name.clone(),
        invocation_checkpoint: approval.invocation_checkpoint,
        arguments_sha256: Sha256::digest(APPROVAL_ARGUMENTS_JSON.as_bytes()).into(),
    }
}

fn create_waiting_approval(
    service: &SessionService,
    session_id: &str,
    key: &str,
    approval_id: &str,
    expires_at: String,
) -> (String, ToolApprovalRequest) {
    let message_id = format!("message_{key}");
    let call_id = format!("call_{key}");
    let run_id = create_started_run(service, session_id, key, &message_id);
    service
        .record_provider_turn(
            &run_id,
            &tool_turn(
                &message_id,
                vec![RawToolCallPlan {
                    call_id: call_id.clone(),
                    tool_name: "terminal".to_owned(),
                    arguments_json: APPROVAL_ARGUMENTS_JSON.to_owned(),
                }],
            ),
        )
        .unwrap();
    service
        .start_tool_invocation_with_event(
            &run_id,
            &call_id,
            "terminal",
            "Run the reviewed test command",
        )
        .unwrap();
    let request = ToolApprovalRequest {
        approval_id: approval_id.to_owned(),
        call_id,
        tool_name: "terminal".to_owned(),
        input_summary: Some("Run the reviewed test command".to_owned()),
        choices: vec![ToolApprovalDecision::Once, ToolApprovalDecision::Deny],
        expires_at,
    };
    service
        .request_tool_approval_with_event(&run_id, &request)
        .unwrap();
    (run_id, request)
}

fn clarification_continuation_binding(
    service: &SessionService,
    run_id: &str,
    request: &ClarificationRequest,
) -> ClarificationContinuationBinding {
    let clarification = service
        .load_clarification(run_id, &request.request_id)
        .unwrap();
    ClarificationContinuationBinding {
        run_id: run_id.to_owned(),
        call_id: request.call_id.clone(),
        invocation_checkpoint: clarification.invocation_checkpoint,
        arguments_sha256: Sha256::digest(CLARIFICATION_ARGUMENTS_JSON.as_bytes()).into(),
    }
}

fn create_waiting_clarification(
    service: &SessionService,
    session_id: &str,
    key: &str,
    request_id: &str,
    choices: Vec<String>,
) -> (String, ClarificationRequest) {
    let message_id = format!("message_{key}");
    let call_id = format!("call_{key}");
    let run_id = create_started_run(service, session_id, key, &message_id);
    service
        .record_provider_turn(
            &run_id,
            &tool_turn(
                &message_id,
                vec![RawToolCallPlan {
                    call_id: call_id.clone(),
                    tool_name: "clarify".to_owned(),
                    arguments_json: CLARIFICATION_ARGUMENTS_JSON.to_owned(),
                }],
            ),
        )
        .unwrap();
    service
        .start_tool_invocation_with_event(
            &run_id,
            &call_id,
            "clarify",
            "Ask the user to choose a target",
        )
        .unwrap();
    let request = ClarificationRequest {
        request_id: request_id.to_owned(),
        call_id,
        question: "Choose a target".to_owned(),
        choices,
    };
    service
        .request_clarification_with_event(&run_id, &request)
        .unwrap();
    (run_id, request)
}

fn run_event_names(service: &SessionService, run_id: &str) -> Vec<String> {
    service
        .run_event_batch(run_id, 0)
        .unwrap()
        .events
        .into_iter()
        .map(|event| event.event_name)
        .collect()
}

fn run_event_data(service: &SessionService, run_id: &str, event_name: &str) -> serde_json::Value {
    let event = service
        .run_event_batch(run_id, 0)
        .unwrap()
        .events
        .into_iter()
        .find(|event| event.event_name == event_name)
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_str(&event.envelope_json).unwrap();
    envelope["data"].clone()
}

#[test]
fn initializes_an_isolated_versioned_store_with_fts5() {
    let fixture = Fixture::new();
    assert!(fixture.service.is_available());
    assert_eq!(
        fixture.service.schema_version(),
        Some(SESSION_SCHEMA_VERSION)
    );
    assert_eq!(fixture.service.search_mode(), SearchMode::Fts5);
    assert!(
        fixture
            .home
            .path()
            .join(".synthchat/sessions-v1.db")
            .is_file()
    );
    assert!(!fixture.home.path().join("state.db").exists());
    let connection =
        rusqlite::Connection::open(fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
    for table in [
        "runs",
        "run_events",
        "run_turns",
        "tool_invocations",
        "run_approvals",
        "run_clarifications",
        "workspaces",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing current table {table}");
    }
    let code_rpc_columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tool_invocations') \
             WHERE name IN ('origin', 'parent_call_id', 'rpc_sequence')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(code_rpc_columns, 3);
    for object in [
        "idx_tool_invocations_code_rpc_sequence",
        "tool_invocation_code_rpc_is_complete",
        "tool_invocation_code_rpc_parent_is_valid",
        "tool_invocation_origin_is_immutable",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
                [object],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing code RPC schema object {object}");
    }
}

#[test]
fn code_rpc_invocations_are_private_sequenced_and_idempotent() {
    let fixture = Fixture::new();
    let session = fixture.create("code RPC journal", "code-rpc-journal-session");
    let message_id = "message_code_rpc_journal";
    let parent_call_id = "call_execute_code_parent";
    let run_id = create_started_run(
        &fixture.service,
        &session.value.id,
        "code-rpc-journal-run",
        message_id,
    );
    fixture
        .service
        .record_provider_turn(
            &run_id,
            &tool_turn(
                message_id,
                vec![RawToolCallPlan {
                    call_id: parent_call_id.to_owned(),
                    tool_name: "execute_code".to_owned(),
                    arguments_json: r#"{"code":"print('private parent code')"}"#.to_owned(),
                }],
            ),
        )
        .unwrap();
    let parent = fixture
        .service
        .start_tool_invocation_with_event(
            &run_id,
            parent_call_id,
            "execute_code",
            "Execute Python programmatic tool script",
        )
        .unwrap();

    let first_arguments = r#"{"path":"private/nested.txt"}"#;
    let first = fixture
        .service
        .plan_code_rpc_invocation(&run_id, parent_call_id, 1, "read_file", first_arguments)
        .unwrap();
    assert_eq!(first.origin, ToolInvocationOrigin::CodeRpc);
    assert_eq!(first.parent_call_id.as_deref(), Some(parent_call_id));
    assert_eq!(first.rpc_sequence, Some(1));
    let replay = fixture
        .service
        .plan_code_rpc_invocation(&run_id, parent_call_id, 1, "read_file", first_arguments)
        .unwrap();
    assert_eq!(replay.call_id, first.call_id);
    assert!(matches!(
        fixture.service.plan_code_rpc_invocation(
            &run_id,
            parent_call_id,
            1,
            "read_file",
            r#"{"path":"changed.txt"}"#,
        ),
        Err(RunError::DataInvalid)
    ));
    assert!(matches!(
        fixture.service.plan_code_rpc_invocation(
            &run_id,
            parent_call_id,
            3,
            "terminal",
            r#"{"command":"echo gap"}"#,
        ),
        Err(RunError::DataInvalid)
    ));
    let first = fixture
        .service
        .start_tool_invocation(&run_id, &first.call_id)
        .unwrap();
    fixture
        .service
        .complete_tool_invocation(
            &run_id,
            &first.call_id,
            first.checkpoint,
            r#"{"content":"private nested result"}"#,
            r#"{"content":"private nested result"}"#,
        )
        .unwrap();

    let second = fixture
        .service
        .plan_code_rpc_invocation(
            &run_id,
            parent_call_id,
            2,
            "terminal",
            r#"{"command":"echo private nested command"}"#,
        )
        .unwrap();
    let second = fixture
        .service
        .start_tool_invocation(&run_id, &second.call_id)
        .unwrap();
    fixture
        .service
        .fail_tool_invocation(
            &run_id,
            &second.call_id,
            second.checkpoint,
            r#"{"code":"private_nested_failure"}"#,
            r#"{"ok":false}"#,
        )
        .unwrap();

    let turns = fixture.service.provider_turns(&run_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].tool_calls.len(), 1);
    assert_eq!(
        turns[0].tool_calls[0].origin,
        ToolInvocationOrigin::Provider
    );
    assert_eq!(turns[0].tool_calls[0].call_id, parent_call_id);
    fixture
        .service
        .complete_tool_invocation(
            &run_id,
            parent_call_id,
            parent.checkpoint,
            r#"{"status":"success","output":"public parent result"}"#,
            r#"{"status":"success","output":"public parent result"}"#,
        )
        .unwrap();
    let context = fixture
        .service
        .provider_continuation_context(&run_id)
        .unwrap();
    let debug_context = format!("{context:?}");
    assert!(debug_context.contains("public parent result"));
    assert!(!debug_context.contains("private/nested.txt"));
    assert!(!debug_context.contains("private nested result"));
    assert!(!debug_context.contains("private nested command"));

    let connection =
        rusqlite::Connection::open(fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let nested_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM tool_invocations WHERE run_id = ?1 AND origin = 'codeRpc'",
            [&run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(nested_count, 2);
}

#[test]
fn migrates_v2_by_adding_run_journal_without_rewriting_messages() {
    let home = tempfile::tempdir().unwrap();
    {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("v2 fixture".to_owned()),
                },
                "v2-fixture-key",
            )
            .unwrap();
        service
            .commit_message(&session.value.id, &text_message("preserve me"))
            .unwrap();
    }
    let db_path = home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP TABLE run_events; DROP TABLE runs; PRAGMA user_version = 2;")
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    let sessions = service
        .list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: false,
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(sessions.items.len(), 1);
    let messages = service
        .list_messages(
            &sessions.items[0].id,
            &ListMessages {
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(messages.items.len(), 1);
}

#[test]
fn migrates_v3_rows_to_current_schema_without_rewriting_the_run_journal() {
    let home = tempfile::tempdir().unwrap();
    let (session_id, run_id, event_count) = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("v3 fixture".to_owned()),
                },
                "v3-fixture-session",
            )
            .unwrap();
        let run_id = create_started_run(
            &service,
            &session.value.id,
            "v3-fixture-run",
            "message_v3_fixture",
        );
        let events = service.run_event_batch(&run_id, 0).unwrap().events.len();
        (session.value.id, run_id, events)
    };
    let db_path = home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE tool_invocations; DROP TABLE run_turns; PRAGMA user_version = 3;",
        )
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    let run = service.get_run(&run_id).unwrap();
    assert_eq!(run.session_id, session_id);
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(
        service.run_event_batch(&run_id, 0).unwrap().events.len(),
        event_count
    );
    assert!(service.provider_turns(&run_id).unwrap().is_empty());
    let connection = rusqlite::Connection::open(db_path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SESSION_SCHEMA_VERSION);
}

#[test]
fn migrates_v5_with_the_approval_ledger_without_rewriting_runs() {
    let home = tempfile::tempdir().unwrap();
    let (run_id, event_count) = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("v5 approval migration".to_owned()),
                },
                "v5-approval-session",
            )
            .unwrap();
        let run_id = create_started_run(
            &service,
            &session.value.id,
            "v5-approval-run",
            "message_v5_approval",
        );
        let event_count = service.run_event_batch(&run_id, 0).unwrap().events.len();
        (run_id, event_count)
    };
    let db_path = home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP TABLE run_approvals; PRAGMA user_version = 5;")
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    assert_eq!(service.get_run(&run_id).unwrap().status, RunStatus::Running);
    assert_eq!(
        service.run_event_batch(&run_id, 0).unwrap().events.len(),
        event_count
    );
    let connection = rusqlite::Connection::open(db_path).unwrap();
    for object in [
        "run_approvals",
        "idx_run_approvals_one_pending",
        "idx_run_approvals_pending_expiry",
        "run_approval_binding_is_complete",
        "run_approval_binding_is_immutable",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
                [object],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing current approval schema object {object}");
    }
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SESSION_SCHEMA_VERSION);
}

#[test]
fn migrates_v7_approval_bindings_from_private_invocation_data() {
    let fixture = Fixture::new();
    let session = fixture.create("v7 approval migration", "v7-approval-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "v7_approval",
        "approval_v7_migration",
        approval_expiry_after(60_000),
    );
    let db_path = fixture.home.path().join(".synthchat/sessions-v1.db");
    drop(fixture.service);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER run_approval_binding_is_complete;
             DROP TRIGGER run_approval_binding_is_immutable;
             DROP INDEX idx_run_approvals_one_pending;
             DROP INDEX idx_run_approvals_pending_expiry;
             ALTER TABLE run_approvals RENAME TO run_approvals_v8;
             CREATE TABLE run_approvals AS SELECT
                approval_id, run_id, call_id, invocation_checkpoint, tool_name,
                input_summary, choices_json, expires_at, expires_at_unix_ms, state,
                decision, reason, resolved_by, created_at, resolved_at, execution_claimed_at
             FROM run_approvals_v8;
             DROP TABLE run_approvals_v8;
             PRAGMA user_version = 7;",
        )
        .unwrap();
    drop(connection);

    let service = SessionService::new(fixture.home.path(), TOKEN);
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    let approval = service
        .load_tool_approval(&run_id, &request.approval_id)
        .unwrap();
    assert_eq!(approval.profile_id, "default");
    assert_eq!(approval.session_id, session.value.id);
    assert_eq!(approval.workspace_id, None);
    let expected_arguments_sha256: [u8; 32] =
        Sha256::digest(APPROVAL_ARGUMENTS_JSON.as_bytes()).into();
    assert_eq!(approval.arguments_sha256, expected_arguments_sha256);
    let connection = rusqlite::Connection::open(db_path).unwrap();
    let binding_columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('run_approvals')
             WHERE name IN ('profile_id', 'session_id', 'workspace_id', 'arguments_sha256')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(binding_columns, 4);
}

#[test]
fn migrates_v8_with_the_bound_immutable_clarification_ledger() {
    let home = tempfile::tempdir().unwrap();
    {
        let service = SessionService::new(home.path(), TOKEN);
        assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    }
    let db_path = home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP TABLE run_clarifications; PRAGMA user_version = 8;")
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    let connection = rusqlite::Connection::open(db_path).unwrap();
    for object in [
        "run_clarifications",
        "idx_run_clarifications_one_pending",
        "run_clarification_request_is_immutable",
        "run_clarification_resolution_is_immutable",
        "run_clarification_claim_is_single_use",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
                [object],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "missing current clarification schema object {object}"
        );
    }
    let columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('run_clarifications') WHERE name IN (
                'request_id', 'run_id', 'call_id', 'invocation_checkpoint',
                'arguments_sha256', 'question', 'choices_json', 'state', 'answer',
                'resolved_by', 'created_at', 'resolved_at', 'continuation_claimed_at'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 13);
}

#[test]
fn migrates_v9_tool_invocations_to_private_code_rpc_journal() {
    let home = tempfile::tempdir().unwrap();
    let (run_id, call_id) = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("v9 code RPC migration".to_owned()),
                },
                "v9-code-rpc-session",
            )
            .unwrap();
        let message_id = "message_v9_code_rpc";
        let run_id = create_started_run(&service, &session.value.id, "v9-code-rpc-run", message_id);
        let call_id = "call_v9_provider".to_owned();
        service
            .record_provider_turn(
                &run_id,
                &tool_turn(
                    message_id,
                    vec![RawToolCallPlan {
                        call_id: call_id.clone(),
                        tool_name: "execute_code".to_owned(),
                        arguments_json: r#"{"code":"print('v9')"}"#.to_owned(),
                    }],
                ),
            )
            .unwrap();
        (run_id, call_id)
    };
    let db_path = home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER tool_invocation_code_rpc_is_complete;
             DROP TRIGGER tool_invocation_code_rpc_parent_is_valid;
             DROP TRIGGER tool_invocation_origin_is_immutable;
             DROP INDEX idx_tool_invocations_code_rpc_sequence;
             DROP TABLE run_approvals;
             DROP TABLE run_clarifications;
             DROP TABLE terminal_processes;
             ALTER TABLE tool_invocations RENAME TO tool_invocations_v10;
             CREATE TABLE tool_invocations (
                run_id TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                call_index INTEGER NOT NULL CHECK(call_index >= 0),
                call_id TEXT NOT NULL CHECK(length(call_id) BETWEEN 1 AND 256),
                tool_name TEXT NOT NULL CHECK(length(tool_name) BETWEEN 1 AND 256),
                arguments_json TEXT NOT NULL
                    CHECK(json_valid(arguments_json) AND json_type(arguments_json) = 'object'),
                status TEXT NOT NULL CHECK(status IN ('planned','running','completed','failed')),
                attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
                checkpoint INTEGER NOT NULL DEFAULT 0 CHECK(checkpoint >= 0),
                result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
                error_json TEXT CHECK(error_json IS NULL OR json_valid(error_json)),
                provider_content TEXT,
                planned_at TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(run_id, call_id),
                UNIQUE(run_id, turn_index, call_index),
                FOREIGN KEY(run_id, turn_index)
                    REFERENCES run_turns(run_id, turn_index) ON DELETE CASCADE,
                CHECK(
                    (status = 'planned' AND attempt = 0 AND checkpoint = 0
                        AND started_at IS NULL AND finished_at IS NULL
                        AND result_json IS NULL AND error_json IS NULL
                        AND provider_content IS NULL)
                    OR (status = 'running' AND attempt >= 1 AND checkpoint >= 1
                        AND started_at IS NOT NULL AND finished_at IS NULL
                        AND result_json IS NULL AND error_json IS NULL
                        AND provider_content IS NULL)
                    OR (status = 'completed' AND attempt >= 1 AND checkpoint >= 2
                        AND started_at IS NOT NULL AND finished_at IS NOT NULL
                        AND result_json IS NOT NULL AND error_json IS NULL
                        AND provider_content IS NOT NULL)
                    OR (status = 'failed' AND attempt >= 1 AND checkpoint >= 2
                        AND started_at IS NOT NULL AND finished_at IS NOT NULL
                        AND result_json IS NULL AND error_json IS NOT NULL
                        AND provider_content IS NOT NULL)
                )
             );
             INSERT INTO tool_invocations(
                run_id, turn_index, call_index, call_id, tool_name, arguments_json,
                status, attempt, checkpoint, result_json, error_json, provider_content,
                planned_at, started_at, finished_at, updated_at
             ) SELECT run_id, turn_index, call_index, call_id, tool_name, arguments_json,
                status, attempt, checkpoint, result_json, error_json, provider_content,
                planned_at, started_at, finished_at, updated_at
               FROM tool_invocations_v10;
             DROP TABLE tool_invocations_v10;
             PRAGMA user_version = 9;",
        )
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    let turns = service.provider_turns(&run_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].tool_calls.len(), 1);
    assert_eq!(turns[0].tool_calls[0].call_id, call_id);
    assert_eq!(
        turns[0].tool_calls[0].origin,
        ToolInvocationOrigin::Provider
    );
    assert_eq!(turns[0].tool_calls[0].parent_call_id, None);
    assert_eq!(turns[0].tool_calls[0].rpc_sequence, None);
    let connection = rusqlite::Connection::open(db_path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SESSION_SCHEMA_VERSION);
}

#[test]
fn clarification_request_is_atomic_durable_idempotent_and_payload_bound() {
    let fixture = Fixture::new();
    let session = fixture.create("clarification request", "clarification-request-session");
    let (run_id, request) = create_waiting_clarification(
        &fixture.service,
        &session.value.id,
        "clarification_request",
        "clarification_request_1",
        vec!["staging".to_owned(), "production".to_owned()],
    );
    let stored = fixture
        .service
        .load_clarification(&run_id, &request.request_id)
        .unwrap();
    assert_eq!(stored.state, ClarificationState::Pending);
    assert_eq!(stored.answer, None);
    assert_eq!(stored.resolved_by, None);
    assert_eq!(stored.invocation_checkpoint, 1);
    let expected_arguments_sha256: [u8; 32] =
        Sha256::digest(CLARIFICATION_ARGUMENTS_JSON.as_bytes()).into();
    assert_eq!(stored.arguments_sha256, expected_arguments_sha256);
    let run = fixture.service.get_run(&run_id).unwrap();
    assert_eq!(run.status, RunStatus::WaitingClarification);
    assert!(matches!(
        run.pending_action,
        Some(PendingAction::Clarification {
            request_id,
            question,
            choices,
        }) if request_id == request.request_id
            && question == request.question
            && choices == request.choices
    ));
    assert_eq!(
        run_event_data(&fixture.service, &run_id, "clarification.required"),
        serde_json::json!({
            "requestId": request.request_id,
            "question": request.question,
            "choices": request.choices,
        })
    );
    let sequence = run.last_sequence;
    assert_eq!(
        fixture
            .service
            .request_clarification_with_event(&run_id, &request)
            .unwrap(),
        stored
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
    let mut changed = request.clone();
    changed.question = "Choose a different target".to_owned();
    assert_eq!(
        fixture
            .service
            .request_clarification_with_event(&run_id, &changed)
            .unwrap_err(),
        ClarificationError::RequestConflict
    );

    let connection =
        rusqlite::Connection::open(fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE run_clarifications SET question = 'mutated' WHERE request_id = ?1",
                [&request.request_id],
            )
            .is_err()
    );

    let session = fixture.create("atomic rejection", "clarification-atomic-rejection");
    let message_id = "message_clarification_atomic_rejection";
    let rejected_run = create_started_run(
        &fixture.service,
        &session.value.id,
        "clarification-atomic-rejection-run",
        message_id,
    );
    let rejected_call = "call_clarification_atomic_rejection";
    fixture
        .service
        .record_provider_turn(
            &rejected_run,
            &tool_turn(
                message_id,
                vec![RawToolCallPlan {
                    call_id: rejected_call.to_owned(),
                    tool_name: "clarify".to_owned(),
                    arguments_json: CLARIFICATION_ARGUMENTS_JSON.to_owned(),
                }],
            ),
        )
        .unwrap();
    fixture
        .service
        .start_tool_invocation(&rejected_run, rejected_call)
        .unwrap();
    let sequence = fixture
        .service
        .get_run(&rejected_run)
        .unwrap()
        .last_sequence;
    let rejected = ClarificationRequest {
        request_id: "clarification_atomic_rejected".to_owned(),
        call_id: rejected_call.to_owned(),
        question: "Choose a target".to_owned(),
        choices: Vec::new(),
    };
    assert_eq!(
        fixture
            .service
            .request_clarification_with_event(&rejected_run, &rejected)
            .unwrap_err(),
        ClarificationError::DataInvalid
    );
    assert_eq!(
        fixture.service.get_run(&rejected_run).unwrap().status,
        RunStatus::Running
    );
    assert_eq!(
        fixture
            .service
            .get_run(&rejected_run)
            .unwrap()
            .last_sequence,
        sequence
    );
    assert_eq!(
        fixture
            .service
            .load_clarification(&rejected_run, &rejected.request_id)
            .unwrap_err(),
        ClarificationError::NotFound
    );
}

#[test]
fn clarification_answer_is_private_exact_idempotent_and_choice_checked() {
    let fixture = Fixture::new();
    let session = fixture.create("clarification answer", "clarification-answer-session");
    let (run_id, request) = create_waiting_clarification(
        &fixture.service,
        &session.value.id,
        "clarification_answer",
        "clarification_answer_1",
        vec!["staging".to_owned(), "production".to_owned()],
    );
    assert_eq!(
        fixture
            .service
            .resolve_clarification(&run_id, &request.request_id, " staging ")
            .unwrap_err(),
        ClarificationError::ChoiceNotOffered
    );
    assert_eq!(
        fixture
            .service
            .load_clarification(&run_id, &request.request_id)
            .unwrap()
            .state,
        ClarificationState::Pending
    );
    let accepted = fixture
        .service
        .resolve_clarification(&run_id, &request.request_id, "staging")
        .unwrap();
    assert_eq!(
        accepted.disposition,
        ClarificationResolutionDisposition::Accepted
    );
    assert_eq!(accepted.clarification.answer.as_deref(), Some("staging"));
    assert_eq!(
        accepted.clarification.resolved_by,
        Some(ClarificationResolvedBy::User)
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().status,
        RunStatus::Running
    );
    assert_eq!(
        run_event_data(&fixture.service, &run_id, "clarification.resolved"),
        serde_json::json!({
            "requestId": request.request_id,
            "resolvedBy": "user",
        })
    );
    let sequence = fixture.service.get_run(&run_id).unwrap().last_sequence;
    assert_eq!(
        fixture
            .service
            .resolve_clarification(&run_id, &request.request_id, "staging")
            .unwrap()
            .disposition,
        ClarificationResolutionDisposition::Replayed
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
    assert_eq!(
        fixture
            .service
            .resolve_clarification(&run_id, &request.request_id, "production")
            .unwrap_err(),
        ClarificationError::AnswerConflict
    );

    let freeform_session = fixture.create("free form", "clarification-freeform-session");
    let (freeform_run, freeform_request) = create_waiting_clarification(
        &fixture.service,
        &freeform_session.value.id,
        "clarification_freeform",
        "clarification_freeform_1",
        Vec::new(),
    );
    let private_answer = "  preserve this answer exactly\n  ";
    let freeform = fixture
        .service
        .resolve_clarification(&freeform_run, &freeform_request.request_id, private_answer)
        .unwrap();
    assert_eq!(
        freeform.clarification.answer.as_deref(),
        Some(private_answer)
    );
    let events = fixture.service.run_event_batch(&freeform_run, 0).unwrap();
    assert!(
        events
            .events
            .iter()
            .all(|event| !event.envelope_json.contains("preserve this answer exactly"))
    );
}

#[test]
fn clarification_answer_claim_is_payload_bound_and_single_use() {
    let fixture = Fixture::new();
    let session = fixture.create("clarification claim", "clarification-claim-session");
    let (run_id, request) = create_waiting_clarification(
        &fixture.service,
        &session.value.id,
        "clarification_claim",
        "clarification_claim_1",
        vec!["staging".to_owned(), "production".to_owned()],
    );
    fixture
        .service
        .resolve_clarification(&run_id, &request.request_id, "staging")
        .unwrap();
    let binding = clarification_continuation_binding(&fixture.service, &run_id, &request);
    let mut wrong = binding.clone();
    wrong.arguments_sha256 = [0xA5; 32];
    assert_eq!(
        fixture
            .service
            .claim_clarification_answer(&run_id, &request.request_id, &wrong)
            .unwrap_err(),
        ClarificationError::ContinuationNotAuthorized
    );
    assert!(
        fixture
            .service
            .load_clarification(&run_id, &request.request_id)
            .unwrap()
            .continuation_claimed_at
            .is_none()
    );
    let claimed = fixture
        .service
        .claim_clarification_answer(&run_id, &request.request_id, &binding)
        .unwrap();
    assert_eq!(claimed.answer.as_deref(), Some("staging"));
    assert!(claimed.continuation_claimed_at.is_some());
    assert_eq!(
        fixture
            .service
            .claim_clarification_answer(&run_id, &request.request_id, &binding)
            .unwrap_err(),
        ClarificationError::ContinuationAlreadyClaimed
    );
}

#[test]
fn cancellation_resolves_a_waiting_clarification_and_tool_atomically() {
    let fixture = Fixture::new();
    let session = fixture.create("clarification cancel", "clarification-cancel-session");
    let (run_id, request) = create_waiting_clarification(
        &fixture.service,
        &session.value.id,
        "clarification_cancel",
        "clarification_cancel_1",
        Vec::new(),
    );
    let (run, disposition) = fixture.service.request_run_cancel(&run_id).unwrap();
    assert_eq!(disposition, CancelDisposition::SignalExecutor);
    assert_eq!(run.status, RunStatus::Cancelling);
    assert!(run.pending_action.is_none());
    let clarification = fixture
        .service
        .load_clarification(&run_id, &request.request_id)
        .unwrap();
    assert_eq!(clarification.state, ClarificationState::Resolved);
    assert_eq!(clarification.answer, None);
    assert_eq!(
        clarification.resolved_by,
        Some(ClarificationResolvedBy::Cancellation)
    );
    assert_eq!(
        fixture.service.provider_turns(&run_id).unwrap()[0].tool_calls[0].status,
        ToolInvocationStatus::Failed
    );
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names[names.len() - 3..],
        [
            "clarification.required",
            "clarification.resolved",
            "tool.failed",
        ]
    );
    assert_eq!(
        fixture
            .service
            .resolve_clarification(&run_id, &request.request_id, "too late")
            .unwrap_err(),
        ClarificationError::NoLongerPending
    );
}

#[test]
fn terminal_failure_resolves_a_waiting_clarification_before_run_failure() {
    let fixture = Fixture::new();
    let session = fixture.create("clarification failure", "clarification-failure-session");
    let (run_id, request) = create_waiting_clarification(
        &fixture.service,
        &session.value.id,
        "clarification_failure",
        "clarification_failure_1",
        Vec::new(),
    );
    let failed = fixture
        .service
        .fail_run(&run_id, &RunProblem::tool(&run_id, &request.call_id))
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    let clarification = fixture
        .service
        .load_clarification(&run_id, &request.request_id)
        .unwrap();
    assert_eq!(
        clarification.resolved_by,
        Some(ClarificationResolvedBy::Failure)
    );
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names[names.len() - 3..],
        ["clarification.resolved", "tool.failed", "run.failed"]
    );
}

#[test]
fn restart_recovery_resolves_clarification_as_failure_before_run_failure() {
    let home = tempfile::tempdir().unwrap();
    let (run_id, request_id) = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("clarification restart".to_owned()),
                },
                "clarification-restart-session",
            )
            .unwrap();
        let (run_id, request) = create_waiting_clarification(
            &service,
            &session.value.id,
            "clarification_restart",
            "clarification_restart_1",
            Vec::new(),
        );
        (run_id, request.request_id)
    };

    let restarted = SessionService::new(home.path(), TOKEN);
    assert_eq!(
        restarted.recover_interrupted_runs().unwrap(),
        vec![run_id.clone()]
    );
    assert_eq!(
        restarted.get_run(&run_id).unwrap().status,
        RunStatus::Failed
    );
    let clarification = restarted.load_clarification(&run_id, &request_id).unwrap();
    assert_eq!(clarification.answer, None);
    assert_eq!(
        clarification.resolved_by,
        Some(ClarificationResolvedBy::Failure)
    );
    let names = run_event_names(&restarted, &run_id);
    assert_eq!(
        names[names.len() - 4..],
        [
            "clarification.required",
            "clarification.resolved",
            "tool.failed",
            "run.failed",
        ]
    );
    assert!(restarted.recover_interrupted_runs().unwrap().is_empty());
}

#[test]
fn approval_request_is_atomic_durable_idempotent_and_payload_bound() {
    let fixture = Fixture::new();
    let session = fixture.create("approval request", "approval-request-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_request",
        "approval_request_1",
        approval_expiry_after(60_000),
    );
    let stored = fixture
        .service
        .load_tool_approval(&run_id, &request.approval_id)
        .unwrap();
    assert_eq!(stored.state, ToolApprovalState::Pending);
    assert_eq!(stored.decision, None);
    assert_eq!(stored.resolved_by, None);
    assert_eq!(stored.invocation_checkpoint, 1);
    let run = fixture.service.get_run(&run_id).unwrap();
    assert_eq!(run.status, RunStatus::WaitingApproval);
    assert!(matches!(
        run.pending_action,
        Some(PendingAction::Approval {
            approval_id,
            call_id,
            choices,
            ..
        }) if approval_id == request.approval_id
            && call_id == request.call_id
            && choices == ["once", "deny"]
    ));
    let sequence = run.last_sequence;
    assert_eq!(
        fixture
            .service
            .request_tool_approval_with_event(&run_id, &request)
            .unwrap(),
        stored
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
    assert_eq!(
        fixture
            .service
            .expire_tool_approval(&run_id, &request.approval_id)
            .unwrap_err(),
        ToolApprovalError::NotExpired
    );

    let mut changed = request.clone();
    changed.input_summary = Some("different public summary".to_owned());
    assert_eq!(
        fixture
            .service
            .request_tool_approval_with_event(&run_id, &changed)
            .unwrap_err(),
        ToolApprovalError::RequestConflict
    );
    let mut duplicate_call = request.clone();
    duplicate_call.approval_id = "approval_request_2".to_owned();
    assert_eq!(
        fixture
            .service
            .request_tool_approval_with_event(&run_id, &duplicate_call)
            .unwrap_err(),
        ToolApprovalError::RequestConflict
    );
    assert_eq!(
        run_event_names(&fixture.service, &run_id)
            .iter()
            .filter(|name| name.as_str() == "approval.required")
            .count(),
        1
    );

    let restarted = SessionService::new(fixture.home.path(), TOKEN);
    assert_eq!(
        restarted
            .load_tool_approval(&run_id, &request.approval_id)
            .unwrap(),
        stored
    );
}

#[test]
fn approval_once_resolution_is_replayable_and_claims_exactly_once() {
    let fixture = Fixture::new();
    let session = fixture.create("approval once", "approval-once-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_once",
        "approval_once_1",
        approval_expiry_after(60_000),
    );
    assert_eq!(
        fixture
            .service
            .resolve_tool_approval(
                &run_id,
                &request.approval_id,
                ToolApprovalDecision::Session,
                None,
            )
            .unwrap_err(),
        ToolApprovalError::ChoiceNotOffered
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().status,
        RunStatus::WaitingApproval
    );

    let resolved = fixture
        .service
        .resolve_tool_approval(
            &run_id,
            &request.approval_id,
            ToolApprovalDecision::Once,
            Some("approved for this invocation"),
        )
        .unwrap();
    assert_eq!(
        resolved.disposition,
        ToolApprovalResolutionDisposition::Accepted
    );
    assert_eq!(resolved.approval.state, ToolApprovalState::Resolved);
    assert_eq!(
        resolved.approval.resolved_by,
        Some(ToolApprovalResolvedBy::User)
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().status,
        RunStatus::Running
    );
    let sequence = fixture.service.get_run(&run_id).unwrap().last_sequence;
    let replay = fixture
        .service
        .resolve_tool_approval(
            &run_id,
            &request.approval_id,
            ToolApprovalDecision::Once,
            Some("approved for this invocation"),
        )
        .unwrap();
    assert_eq!(
        replay.disposition,
        ToolApprovalResolutionDisposition::Replayed
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
    assert_eq!(
        fixture
            .service
            .resolve_tool_approval(
                &run_id,
                &request.approval_id,
                ToolApprovalDecision::Once,
                None,
            )
            .unwrap_err(),
        ToolApprovalError::DecisionConflict
    );

    let binding = approval_execution_binding(&fixture.service, &run_id, &request);
    let mismatched_bindings = [
        ToolApprovalExecutionBinding {
            run_id: "run_stale".to_owned(),
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            profile_id: "different-profile".to_owned(),
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            session_id: "session_stale".to_owned(),
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            workspace_id: Some("workspace_stale".to_owned()),
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            call_id: "call_stale".to_owned(),
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            tool_name: "workspace_write_file".to_owned(),
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            invocation_checkpoint: binding.invocation_checkpoint + 1,
            ..binding.clone()
        },
        ToolApprovalExecutionBinding {
            arguments_sha256: [0; 32],
            ..binding.clone()
        },
    ];
    for mismatched in mismatched_bindings {
        assert_eq!(
            fixture
                .service
                .claim_tool_approval(&run_id, &request.approval_id, &mismatched,),
            Err(ToolApprovalError::ExecutionNotAuthorized)
        );
    }
    let db_path = fixture.home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE run_approvals SET profile_id = 'tampered' WHERE approval_id = ?1",
                [&request.approval_id],
            )
            .is_err()
    );
    connection
        .execute(
            "UPDATE tool_invocations SET arguments_json = '{\"command\":\"changed\"}'
             WHERE run_id = ?1 AND call_id = ?2",
            rusqlite::params![run_id, request.call_id],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        fixture
            .service
            .claim_tool_approval(&run_id, &request.approval_id, &binding),
        Err(ToolApprovalError::ExecutionNotAuthorized)
    );
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .execute(
            "UPDATE tool_invocations SET arguments_json = ?1
             WHERE run_id = ?2 AND call_id = ?3",
            rusqlite::params![APPROVAL_ARGUMENTS_JSON, run_id, request.call_id],
        )
        .unwrap();
    drop(connection);
    assert!(
        fixture
            .service
            .load_tool_approval(&run_id, &request.approval_id)
            .unwrap()
            .execution_claimed_at
            .is_none()
    );
    let claimed = fixture
        .service
        .claim_tool_approval(&run_id, &request.approval_id, &binding)
        .unwrap();
    assert!(claimed.execution_claimed_at.is_some());
    assert_eq!(
        fixture
            .service
            .claim_tool_approval(&run_id, &request.approval_id, &binding)
            .unwrap_err(),
        ToolApprovalError::ExecutionAlreadyClaimed
    );
    let restarted = SessionService::new(fixture.home.path(), TOKEN);
    let restarted_binding = approval_execution_binding(&restarted, &run_id, &request);
    assert_eq!(
        restarted
            .claim_tool_approval(&run_id, &request.approval_id, &restarted_binding)
            .unwrap_err(),
        ToolApprovalError::ExecutionAlreadyClaimed
    );
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names[names.len() - 2..],
        ["approval.required", "approval.resolved"]
    );
}

#[test]
fn approval_claim_is_linearizable_across_service_instances() {
    let fixture = Fixture::new();
    let session = fixture.create("approval claim race", "approval-claim-race-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_claim_race",
        "approval_claim_race_1",
        approval_expiry_after(60_000),
    );
    fixture
        .service
        .resolve_tool_approval(
            &run_id,
            &request.approval_id,
            ToolApprovalDecision::Once,
            None,
        )
        .unwrap();
    let sequence = fixture.service.get_run(&run_id).unwrap().last_sequence;
    let binding = approval_execution_binding(&fixture.service, &run_id, &request);
    let services = [
        fixture.service.clone(),
        SessionService::new(fixture.home.path(), TOKEN),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let handles = services.map(|service| {
            let barrier = Arc::clone(&barrier);
            let run_id = run_id.clone();
            let approval_id = request.approval_id.clone();
            let binding = binding.clone();
            scope.spawn(move || {
                barrier.wait();
                service.claim_tool_approval(&run_id, &approval_id, &binding)
            })
        });
        handles.map(|handle| handle.join().unwrap())
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| { matches!(result, Err(ToolApprovalError::ExecutionAlreadyClaimed)) })
            .count(),
        1
    );
    assert!(
        fixture
            .service
            .load_tool_approval(&run_id, &request.approval_id)
            .unwrap()
            .execution_claimed_at
            .is_some()
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
}

#[test]
fn approval_deny_is_an_immutable_replayable_tool_failure() {
    let fixture = Fixture::new();
    let session = fixture.create("approval deny", "approval-deny-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_deny",
        "approval_deny_1",
        approval_expiry_after(60_000),
    );
    let resolved = fixture
        .service
        .resolve_tool_approval(
            &run_id,
            &request.approval_id,
            ToolApprovalDecision::Deny,
            Some("command not approved"),
        )
        .unwrap();
    assert_eq!(
        resolved.disposition,
        ToolApprovalResolutionDisposition::Accepted
    );
    assert_eq!(resolved.approval.decision, Some(ToolApprovalDecision::Deny));
    assert_eq!(
        resolved.approval.resolved_by,
        Some(ToolApprovalResolvedBy::User)
    );
    assert_eq!(
        resolved.approval.reason.as_deref(),
        Some("command not approved")
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().status,
        RunStatus::Running
    );
    assert!(
        fixture
            .service
            .unfinished_tool_invocations(&run_id)
            .unwrap()
            .is_empty()
    );
    let sequence = fixture.service.get_run(&run_id).unwrap().last_sequence;
    let replay = fixture
        .service
        .resolve_tool_approval(
            &run_id,
            &request.approval_id,
            ToolApprovalDecision::Deny,
            Some("command not approved"),
        )
        .unwrap();
    assert_eq!(
        replay.disposition,
        ToolApprovalResolutionDisposition::Replayed
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
    assert_eq!(
        fixture
            .service
            .resolve_tool_approval(
                &run_id,
                &request.approval_id,
                ToolApprovalDecision::Once,
                Some("command not approved"),
            )
            .unwrap_err(),
        ToolApprovalError::DecisionConflict
    );
    assert_eq!(
        fixture
            .service
            .claim_tool_approval(
                &run_id,
                &request.approval_id,
                &approval_execution_binding(&fixture.service, &run_id, &request),
            )
            .unwrap_err(),
        ToolApprovalError::ExecutionNotAuthorized
    );
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names[names.len() - 3..],
        ["approval.required", "approval.resolved", "tool.failed"]
    );
}

#[test]
fn expired_approval_fails_closed_before_returning_the_conflict() {
    let fixture = Fixture::new();
    let session = fixture.create("approval expiry", "approval-expiry-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_expiry",
        "approval_expiry_1",
        approval_expiry_after(1_000),
    );
    std::thread::sleep(std::time::Duration::from_millis(1_100));

    assert_eq!(
        fixture
            .service
            .resolve_tool_approval(
                &run_id,
                &request.approval_id,
                ToolApprovalDecision::Once,
                None,
            )
            .unwrap_err(),
        ToolApprovalError::Expired
    );
    let approval = fixture
        .service
        .load_tool_approval(&run_id, &request.approval_id)
        .unwrap();
    assert_eq!(approval.state, ToolApprovalState::Resolved);
    assert_eq!(approval.decision, Some(ToolApprovalDecision::Deny));
    assert_eq!(approval.resolved_by, Some(ToolApprovalResolvedBy::Expiry));
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().status,
        RunStatus::Running
    );
    let invocation = fixture
        .service
        .unfinished_tool_invocations(&run_id)
        .unwrap();
    assert!(invocation.is_empty());
    let sequence = fixture.service.get_run(&run_id).unwrap().last_sequence;
    let replay = fixture
        .service
        .expire_tool_approval(&run_id, &request.approval_id)
        .unwrap();
    assert_eq!(
        replay.disposition,
        ToolApprovalResolutionDisposition::Replayed
    );
    assert_eq!(
        fixture.service.get_run(&run_id).unwrap().last_sequence,
        sequence
    );
    assert_eq!(
        fixture
            .service
            .claim_tool_approval(
                &run_id,
                &request.approval_id,
                &approval_execution_binding(&fixture.service, &run_id, &request),
            )
            .unwrap_err(),
        ToolApprovalError::ExecutionNotAuthorized
    );
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names[names.len() - 3..],
        ["approval.required", "approval.resolved", "tool.failed"]
    );
}

#[test]
fn cancellation_resolves_a_waiting_approval_and_tool_in_one_transaction() {
    let fixture = Fixture::new();
    let session = fixture.create("approval cancel", "approval-cancel-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_cancel",
        "approval_cancel_1",
        approval_expiry_after(60_000),
    );
    let (run, disposition) = fixture.service.request_run_cancel(&run_id).unwrap();
    assert_eq!(disposition, CancelDisposition::SignalExecutor);
    assert_eq!(run.status, RunStatus::Cancelling);
    assert!(run.pending_action.is_none());
    let approval = fixture
        .service
        .load_tool_approval(&run_id, &request.approval_id)
        .unwrap();
    assert_eq!(approval.decision, Some(ToolApprovalDecision::Deny));
    assert_eq!(
        approval.resolved_by,
        Some(ToolApprovalResolvedBy::Cancellation)
    );
    assert_eq!(
        fixture
            .service
            .resolve_tool_approval(
                &run_id,
                &request.approval_id,
                ToolApprovalDecision::Deny,
                None,
            )
            .unwrap_err(),
        ToolApprovalError::NoLongerPending
    );
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names[names.len() - 3..],
        ["approval.required", "approval.resolved", "tool.failed"]
    );
    let terminal = fixture
        .service
        .cancel_run_terminal(&run_id, "cancelled by test")
        .unwrap();
    assert_eq!(terminal.status, RunStatus::Cancelled);
    assert_eq!(
        run_event_names(&fixture.service, &run_id).last().unwrap(),
        "run.cancelled"
    );
}

#[test]
fn approval_deny_and_cancel_race_has_one_immutable_resolution_and_one_tool_terminal() {
    let fixture = Fixture::new();
    let session = fixture.create("approval race", "approval-race-session");
    let (run_id, request) = create_waiting_approval(
        &fixture.service,
        &session.value.id,
        "approval_race",
        "approval_race_1",
        approval_expiry_after(60_000),
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let concurrent_service = SessionService::new(fixture.home.path(), TOKEN);
    let resolver = {
        let service = fixture.service.clone();
        let run_id = run_id.clone();
        let approval_id = request.approval_id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            service.resolve_tool_approval(
                &run_id,
                &approval_id,
                ToolApprovalDecision::Deny,
                Some("deny the command"),
            )
        })
    };
    let canceller = {
        let service = concurrent_service;
        let run_id = run_id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            service.request_run_cancel(&run_id)
        })
    };
    barrier.wait();
    let resolution = resolver.join().unwrap();
    let cancellation = canceller.join().unwrap().unwrap();
    assert_eq!(cancellation.0.status, RunStatus::Cancelling);
    let approval = fixture
        .service
        .load_tool_approval(&run_id, &request.approval_id)
        .unwrap();
    match approval.resolved_by {
        Some(ToolApprovalResolvedBy::User) => {
            assert!(resolution.is_ok());
            assert_eq!(approval.reason.as_deref(), Some("deny the command"));
        }
        Some(ToolApprovalResolvedBy::Cancellation) => {
            assert_eq!(resolution.unwrap_err(), ToolApprovalError::NoLongerPending);
            assert_eq!(approval.reason, None);
        }
        other => panic!("unexpected race winner: {other:?}"),
    }
    let names = run_event_names(&fixture.service, &run_id);
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "approval.resolved")
            .count(),
        1
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "tool.failed")
            .count(),
        1
    );
}

#[test]
fn restart_recovery_completes_the_pending_approval_event_chain_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let (run_id, approval_id) = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("approval restart".to_owned()),
                },
                "approval-restart-session",
            )
            .unwrap();
        let (run_id, request) = create_waiting_approval(
            &service,
            &session.value.id,
            "approval_restart",
            "approval_restart_1",
            approval_expiry_after(60_000),
        );
        (run_id, request.approval_id)
    };

    let restarted = SessionService::new(home.path(), TOKEN);
    assert_eq!(
        restarted.recover_interrupted_runs().unwrap(),
        vec![run_id.clone()]
    );
    assert_eq!(
        restarted.get_run(&run_id).unwrap().status,
        RunStatus::Failed
    );
    let approval = restarted.load_tool_approval(&run_id, &approval_id).unwrap();
    assert_eq!(approval.decision, Some(ToolApprovalDecision::Deny));
    assert_eq!(
        approval.resolved_by,
        Some(ToolApprovalResolvedBy::Cancellation)
    );
    let names = run_event_names(&restarted, &run_id);
    assert_eq!(
        names[names.len() - 4..],
        [
            "approval.required",
            "approval.resolved",
            "tool.failed",
            "run.failed"
        ]
    );
    let sequence = restarted.get_run(&run_id).unwrap().last_sequence;
    assert!(restarted.recover_interrupted_runs().unwrap().is_empty());
    assert_eq!(restarted.get_run(&run_id).unwrap().last_sequence, sequence);
}

#[test]
fn duplicate_tool_call_ids_reject_the_whole_provider_plan() {
    let fixture = Fixture::new();
    let session = fixture.create("duplicate calls", "duplicate-session");
    let message_id = "message_duplicate_calls";
    let run_id = create_started_run(
        &fixture.service,
        &session.value.id,
        "duplicate-run-key",
        message_id,
    );
    let duplicate = tool_turn(
        message_id,
        vec![
            RawToolCallPlan {
                call_id: "call-duplicate".to_owned(),
                tool_name: "terminal".to_owned(),
                arguments_json: "{\"command\":\"first\"}".to_owned(),
            },
            RawToolCallPlan {
                call_id: "call-duplicate".to_owned(),
                tool_name: "terminal".to_owned(),
                arguments_json: "{\"command\":\"second\"}".to_owned(),
            },
        ],
    );

    assert!(matches!(
        fixture.service.record_provider_turn(&run_id, &duplicate),
        Err(RunError::DataInvalid)
    ));
    assert!(fixture.service.provider_turns(&run_id).unwrap().is_empty());
    assert!(
        fixture
            .service
            .unfinished_tool_invocations(&run_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn tool_journal_cas_and_restart_preserve_raw_provider_context() {
    let home = tempfile::tempdir().unwrap();
    let message_id = "message_tool_journal";
    let (run_id, first_checkpoint) = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("tool journal".to_owned()),
                },
                "tool-journal-session",
            )
            .unwrap();
        let run_id =
            create_started_run(&service, &session.value.id, "tool-journal-run", message_id);
        let arguments_a = "{  \"path\": \"first\", \"path\": \"second\" }";
        let plan = tool_turn(
            message_id,
            vec![
                RawToolCallPlan {
                    call_id: "call-a".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments_json: arguments_a.to_owned(),
                },
                RawToolCallPlan {
                    call_id: "call-b".to_owned(),
                    tool_name: "terminal".to_owned(),
                    arguments_json: "{\"command\":\"cargo test\"}".to_owned(),
                },
            ],
        );
        let stored = service.record_provider_turn(&run_id, &plan).unwrap();
        assert_eq!(stored.tool_calls[0].arguments_json, arguments_a);
        assert_eq!(stored.tool_calls[0].call_index, 0);
        assert_eq!(stored.tool_calls[1].call_index, 1);
        let pending_context = service.provider_continuation_context(&run_id);
        assert!(
            matches!(pending_context, Err(RunError::DataInvalid)),
            "unexpected pending context result: {pending_context:?}"
        );

        let first = service.start_tool_invocation(&run_id, "call-a").unwrap();
        assert_eq!(first.status, ToolInvocationStatus::Running);
        assert_eq!(first.attempt, 1);
        assert_eq!(first.checkpoint, 1);
        assert!(matches!(
            service.start_tool_invocation(&run_id, "call-a"),
            Err(RunError::DataInvalid)
        ));
        assert!(matches!(
            service.complete_tool_invocation(
                &run_id,
                "call-a",
                first.checkpoint + 1,
                "{\"ok\":false}",
                "wrong checkpoint",
            ),
            Err(RunError::DataInvalid)
        ));
        let result_a = "{ \"text\": \"preserve spacing\" }";
        let completed = service
            .complete_tool_invocation(
                &run_id,
                "call-a",
                first.checkpoint,
                result_a,
                "file contents",
            )
            .unwrap();
        assert_eq!(completed.status, ToolInvocationStatus::Completed);
        assert_eq!(completed.checkpoint, first.checkpoint + 1);
        assert_eq!(completed.result_json.as_deref(), Some(result_a));
        assert!(matches!(
            service.complete_tool_invocation(
                &run_id,
                "call-a",
                first.checkpoint,
                result_a,
                "duplicate",
            ),
            Err(RunError::DataInvalid)
        ));

        let second = service.start_tool_invocation(&run_id, "call-b").unwrap();
        let raw_error = "{\"code\":\"permission_denied\", \"retryable\":false}";
        let failed = service
            .fail_tool_invocation(
                &run_id,
                "call-b",
                second.checkpoint,
                raw_error,
                "permission denied",
            )
            .unwrap();
        assert_eq!(failed.status, ToolInvocationStatus::Failed);
        assert_eq!(failed.error_json.as_deref(), Some(raw_error));
        assert!(
            service
                .unfinished_tool_invocations(&run_id)
                .unwrap()
                .is_empty()
        );
        (run_id, first.checkpoint)
    };

    let service = SessionService::new(home.path(), TOKEN);
    let turns = service.provider_turns(&run_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].tool_calls[0].checkpoint, first_checkpoint + 1);
    assert_eq!(
        turns[0].tool_calls[0].arguments_json,
        "{  \"path\": \"first\", \"path\": \"second\" }"
    );
    assert_eq!(
        turns[0].tool_calls[0].result_json.as_deref(),
        Some("{ \"text\": \"preserve spacing\" }")
    );
    let context = service.provider_continuation_context(&run_id).unwrap();
    assert_eq!(context.len(), 4);
    assert!(matches!(
        &context[0],
        ProviderContextMessage::User { content } if content == "invoke tools"
    ));
    assert!(matches!(
        &context[1],
        ProviderContextMessage::Assistant { tool_calls, .. }
            if tool_calls.iter().map(|call| call.call_id.as_str()).collect::<Vec<_>>()
                == vec!["call-a", "call-b"]
    ));
    assert!(matches!(
        &context[2],
        ProviderContextMessage::Tool { tool_call_id, content }
            if tool_call_id == "call-a" && content == "file contents"
    ));
    assert!(matches!(
        &context[3],
        ProviderContextMessage::Tool { tool_call_id, content }
            if tool_call_id == "call-b" && content == "permission denied"
    ));

    let duplicate_across_turns = ProviderTurnPlan {
        turn_index: 2,
        assistant_message_id: message_id.to_owned(),
        content: None,
        reasoning: None,
        finish: ProviderTurnFinish::ToolCalls,
        usage: Usage {
            prompt_tokens: 20,
            completion_tokens: 3,
            total_tokens: 23,
            cost: None,
        },
        tool_calls: vec![RawToolCallPlan {
            call_id: "call-a".to_owned(),
            tool_name: "read_file".to_owned(),
            arguments_json: "{}".to_owned(),
        }],
    };
    assert!(matches!(
        service.record_provider_turn(&run_id, &duplicate_across_turns),
        Err(RunError::DataInvalid)
    ));
    let stop = ProviderTurnPlan {
        turn_index: 2,
        assistant_message_id: message_id.to_owned(),
        content: Some("done".to_owned()),
        reasoning: None,
        finish: ProviderTurnFinish::Stop,
        usage: Usage {
            prompt_tokens: 20,
            completion_tokens: 1,
            total_tokens: 21,
            cost: None,
        },
        tool_calls: Vec::new(),
    };
    service.record_provider_turn(&run_id, &stop).unwrap();
    let illegal_third = ProviderTurnPlan {
        turn_index: 3,
        ..stop
    };
    assert!(matches!(
        service.record_provider_turn(&run_id, &illegal_third),
        Err(RunError::DataInvalid)
    ));
}

#[test]
fn recovered_running_invocation_is_retained_but_cannot_be_reclaimed() {
    let home = tempfile::tempdir().unwrap();
    let message_id = "message_interrupted_tool";
    let run_id = {
        let service = SessionService::new(home.path(), TOKEN);
        let session = service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("interrupted tool".to_owned()),
                },
                "interrupted-session",
            )
            .unwrap();
        let run_id = create_started_run(&service, &session.value.id, "interrupted-run", message_id);
        service
            .record_provider_turn(
                &run_id,
                &tool_turn(
                    message_id,
                    vec![RawToolCallPlan {
                        call_id: "call-side-effect".to_owned(),
                        tool_name: "terminal".to_owned(),
                        arguments_json: "{\"command\":\"do-not-repeat\"}".to_owned(),
                    }],
                ),
            )
            .unwrap();
        service
            .start_tool_invocation(&run_id, "call-side-effect")
            .unwrap();
        run_id
    };

    let service = SessionService::new(home.path(), TOKEN);
    assert_eq!(
        service.recover_interrupted_runs().unwrap(),
        vec![run_id.clone()]
    );
    assert_eq!(service.get_run(&run_id).unwrap().status, RunStatus::Failed);
    let unfinished = service.unfinished_tool_invocations(&run_id).unwrap();
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].status, ToolInvocationStatus::Running);
    assert_eq!(unfinished[0].attempt, 1);
    assert_eq!(unfinished[0].checkpoint, 1);
    assert_eq!(
        unfinished[0].arguments_json,
        "{\"command\":\"do-not-repeat\"}"
    );
    assert!(matches!(
        service.start_tool_invocation(&run_id, "call-side-effect"),
        Err(RunError::DataInvalid)
    ));
    assert!(matches!(
        service.complete_tool_invocation(
            &run_id,
            "call-side-effect",
            unfinished[0].checkpoint,
            "{\"ok\":true}",
            "must not commit",
        ),
        Err(RunError::DataInvalid)
    ));
}

#[test]
fn workspace_registration_is_opaque_idempotent_profile_scoped_and_run_bound() {
    let fixture = Fixture::new();
    let root = fixture.home.path().join("approved-root");
    std::fs::create_dir(&root).unwrap();
    let root_text = root.to_str().unwrap();
    let workspace = fixture
        .service
        .register_workspace("default", root_text, "workspace-create-key")
        .unwrap();
    assert!(workspace.id.starts_with("workspace_"));
    assert_eq!(workspace.profile_id, "default");
    assert_eq!(workspace.display_name, "approved-root");
    assert!(workspace.available);
    assert!(
        !serde_json::to_string(&workspace)
            .unwrap()
            .contains(root_text)
    );
    assert_eq!(
        fixture
            .service
            .register_workspace("default", root_text, "workspace-create-key")
            .unwrap(),
        workspace
    );

    let other_root = fixture.home.path().join("other-root");
    std::fs::create_dir(&other_root).unwrap();
    assert!(matches!(
        fixture.service.register_workspace(
            "default",
            other_root.to_str().unwrap(),
            "workspace-create-key",
        ),
        Err(SessionError::IdempotencyConflict)
    ));
    assert_eq!(
        fixture.service.list_workspaces("default").unwrap(),
        std::slice::from_ref(&workspace)
    );

    let session = fixture.create("workspace run", "workspace-session");
    let accepted = fixture
        .service
        .create_run(
            &session.value.id,
            &CreateRun {
                client_request_id: "workspace-run".to_owned(),
                message: ChatInput {
                    text: "use the workspace".to_owned(),
                    file_ids: Vec::new(),
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: Some(workspace.id.clone()),
            },
            "workspace-run-key",
            "test/model",
        )
        .unwrap();
    assert_eq!(
        fixture
            .service
            .workspace_path_for_run(&accepted.run.id)
            .unwrap(),
        Some(std::fs::canonicalize(&root).unwrap())
    );
    assert!(matches!(
        fixture.service.delete_workspace("default", &workspace.id),
        Err(SessionError::WorkspaceInUse)
    ));

    let foreign = fixture
        .service
        .register_workspace("other", other_root.to_str().unwrap(), "foreign-workspace")
        .unwrap();
    let second = fixture.create("foreign workspace", "foreign-workspace-session");
    assert!(matches!(
        fixture.service.create_run(
            &second.value.id,
            &CreateRun {
                client_request_id: "foreign-run".to_owned(),
                message: ChatInput {
                    text: "no".to_owned(),
                    file_ids: Vec::new()
                },
                model_override: None,
                reasoning_effort: None,
                workspace_id: Some(foreign.id),
            },
            "foreign-run-key",
            "test/model",
        ),
        Err(RunError::InvalidRequest)
    ));
}

#[test]
fn concurrent_tool_claim_has_exactly_one_winner_and_raw_io_stays_internal() {
    let home = tempfile::tempdir().unwrap();
    let service = SessionService::new(home.path(), TOKEN);
    let session = service
        .create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("concurrent claim".to_owned()),
            },
            "concurrent-session",
        )
        .unwrap();
    let message_id = "message_concurrent_claim";
    let run_id = create_started_run(&service, &session.value.id, "concurrent-run", message_id);
    let secret = "secret-must-stay-in-journal";
    service
        .record_provider_turn(
            &run_id,
            &tool_turn(
                message_id,
                vec![RawToolCallPlan {
                    call_id: "call-race".to_owned(),
                    tool_name: "terminal".to_owned(),
                    arguments_json: format!("{{\"command\":\"{secret}\"}}"),
                }],
            ),
        )
        .unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let home_path = home.path().to_path_buf();
    let mut handles = Vec::new();
    for _ in 0..2 {
        let contender_home = home_path.clone();
        let contender_run_id = run_id.clone();
        let contender_barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let contender = SessionService::new(&contender_home, TOKEN);
            contender_barrier.wait();
            contender.start_tool_invocation(&contender_run_id, "call-race")
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(RunError::DataInvalid)))
            .count(),
        1
    );
    let invocation = service
        .unfinished_tool_invocations(&run_id)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(invocation.status, ToolInvocationStatus::Running);
    assert_eq!(invocation.attempt, 1);
    assert_eq!(invocation.checkpoint, 1);

    let public_run = serde_json::to_string(&service.get_run(&run_id).unwrap()).unwrap();
    assert!(!public_run.contains(secret));
    let public_events = service.run_event_batch(&run_id, 0).unwrap();
    assert!(
        public_events
            .events
            .iter()
            .all(|event| !event.envelope_json.contains(secret))
    );
    let messages = service
        .list_messages(
            &session.value.id,
            &ListMessages {
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
    assert!(
        messages
            .items
            .iter()
            .all(|message| !serde_json::to_string(message).unwrap().contains(secret))
    );
}

#[test]
fn migrates_v1_messages_with_context_enabled_by_default() {
    let home = tempfile::tempdir().unwrap();
    let data_dir = home.path().join(".synthchat");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db_path = data_dir.join("sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA user_version = 1; \
             CREATE TABLE messages (\
               row_id INTEGER PRIMARY KEY AUTOINCREMENT,\
               id TEXT NOT NULL UNIQUE,\
               session_id TEXT NOT NULL,\
               sequence INTEGER NOT NULL,\
               role TEXT NOT NULL,\
               parts_json TEXT NOT NULL,\
               reasoning TEXT,\
               tool_calls_json TEXT NOT NULL,\
               searchable_text TEXT NOT NULL,\
               created_at TEXT NOT NULL,\
               committed_change INTEGER NOT NULL,\
               UNIQUE(session_id, sequence)\
             );\
             INSERT INTO messages(\
               id, session_id, sequence, role, parts_json, tool_calls_json,\
               searchable_text, created_at, committed_change\
             ) VALUES(\
               'legacy-message', 'legacy-session', 1, 'user', '[]', '[]', '',\
               '2026-07-16T00:00:00Z', 1\
             );",
        )
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert!(service.is_available());
    assert_eq!(service.schema_version(), Some(SESSION_SCHEMA_VERSION));
    let connection = rusqlite::Connection::open(db_path).unwrap();
    let context_eligible: i64 = connection
        .query_row(
            "SELECT context_eligible FROM messages WHERE id = 'legacy-message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(context_eligible, 1);
}

#[test]
fn existing_schema_repairs_roll_back_as_one_transaction() {
    let home = tempfile::tempdir().unwrap();
    let data_dir = home.path().join(".synthchat");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db_path = data_dir.join("sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA user_version = 1; \
             CREATE TABLE session_versions (incompatible_column TEXT);",
        )
        .unwrap();
    drop(connection);

    let service = SessionService::new(home.path(), TOKEN);
    assert!(!service.is_available());

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let created_objects: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'table' AND name IN ('app_meta', 'sessions')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_objects, 0);
}

#[test]
fn create_update_delete_and_idempotent_replay_preserve_contract_boundaries() {
    let fixture = Fixture::new();
    let created = fixture.create("  Design notes  ", "create-design-notes");
    assert_eq!(created.value.title, "Design notes");
    assert_eq!(created.etag, format!("\"{}\"", created.value.revision));

    let replay = fixture.create("Design notes", "create-design-notes");
    assert_eq!(replay, created);
    assert!(matches!(
        fixture.service.create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("Different".to_owned()),
            },
            "create-design-notes",
        ),
        Err(SessionError::IdempotencyConflict)
    ));

    let no_op = fixture
        .service
        .update_session(
            &created.value.id,
            &created.etag,
            &SessionPatch {
                title: PatchField::Value("Design notes".to_owned()),
                archived: PatchField::Missing,
            },
        )
        .unwrap();
    assert_eq!(no_op.etag, created.etag);

    let updated = fixture
        .service
        .update_session(
            &created.value.id,
            &created.etag,
            &SessionPatch {
                title: PatchField::Value("Architecture".to_owned()),
                archived: PatchField::Missing,
            },
        )
        .unwrap();
    assert_ne!(updated.etag, created.etag);
    assert!(matches!(
        fixture.service.update_session(
            &created.value.id,
            &created.etag,
            &SessionPatch {
                title: PatchField::Value("Architecture".to_owned()),
                archived: PatchField::Missing,
            },
        ),
        Err(SessionError::RevisionConflict { .. })
    ));
    assert!(matches!(
        fixture.service.delete_session(&created.value.id, None),
        Err(SessionError::PreconditionRequired)
    ));
    fixture
        .service
        .delete_session(&created.value.id, Some(&updated.etag))
        .unwrap();
    fixture
        .service
        .delete_session(&created.value.id, None)
        .unwrap();
    assert!(matches!(
        fixture.service.create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("Design notes".to_owned()),
            },
            "create-design-notes",
        ),
        Err(SessionError::IdempotentResourceDeleted)
    ));
}

#[test]
fn session_cursor_is_filter_bound_tamper_evident_and_as_of_updates() {
    let fixture = Fixture::new();
    let first = fixture.create("First", "create-first-session");
    let second = fixture.create("Second", "create-second-session");
    let third = fixture.create("Third", "create-third-session");

    let page = fixture
        .service
        .list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: false,
            cursor: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(page.items[0].id, third.value.id);
    let cursor = page.next_cursor.unwrap();

    let updated_second = fixture
        .service
        .update_session(
            &second.value.id,
            &second.etag,
            &SessionPatch {
                title: PatchField::Value("Second, updated later".to_owned()),
                archived: PatchField::Missing,
            },
        )
        .unwrap();
    let fourth = fixture.create("Fourth", "create-fourth-session");

    let next = fixture
        .service
        .list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: false,
            cursor: Some(cursor.clone()),
            limit: 10,
        })
        .unwrap();
    assert_eq!(
        next.items
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        vec![second.value.id.as_str(), first.value.id.as_str()]
    );
    assert_eq!(next.items[0].title, "Second");
    assert!(
        !next
            .items
            .iter()
            .any(|session| session.id == fourth.value.id)
    );
    assert_eq!(
        fixture
            .service
            .get_session(&second.value.id)
            .unwrap()
            .value
            .title,
        updated_second.value.title
    );

    let mut tampered = cursor.clone().into_bytes();
    let last = tampered.last_mut().unwrap();
    *last = if *last == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(tampered).unwrap();
    assert!(matches!(
        fixture.service.list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: false,
            cursor: Some(tampered),
            limit: 10,
        }),
        Err(SessionError::InvalidCursor)
    ));
    assert!(matches!(
        fixture.service.list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: true,
            cursor: Some(cursor),
            limit: 10,
        }),
        Err(SessionError::InvalidCursor)
    ));
}

#[test]
fn search_is_literal_and_reports_javascript_utf16_ranges() {
    let fixture = Fixture::new();
    let title = fixture.create("A😀B", "create-unicode-session");
    let message_session = fixture.create("Messages", "create-message-session");
    fixture
        .service
        .commit_message(
            &message_session.value.id,
            &text_message("literal %_ NEAR * \" phrase"),
        )
        .unwrap();

    let title_page = fixture
        .service
        .list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: Some("😀".to_owned()),
            archived: false,
            cursor: None,
            limit: 30,
        })
        .unwrap();
    let matched = title_page
        .items
        .iter()
        .find(|session| session.id == title.value.id)
        .unwrap();
    let search_match = matched.search_match.as_ref().unwrap();
    assert_eq!(search_match.field, SearchField::Title);
    assert_eq!(search_match.ranges[0].start, 1);
    assert_eq!(search_match.ranges[0].end, 3);

    let message_page = fixture
        .service
        .list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: Some("%_ NEAR * \"".to_owned()),
            archived: false,
            cursor: None,
            limit: 30,
        })
        .unwrap();
    let matched = message_page
        .items
        .iter()
        .find(|session| session.id == message_session.value.id)
        .unwrap();
    assert_eq!(
        matched.search_match.as_ref().unwrap().field,
        SearchField::Message
    );
}

#[test]
fn message_pages_keep_the_initial_snapshot_while_new_messages_arrive() {
    let fixture = Fixture::new();
    let session = fixture.create("History", "create-history-session");
    for sequence in 1..=5 {
        let mut request = text_message(&format!("message {sequence}"));
        if sequence == 5 {
            request.usage = Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost: Some(0.01),
            });
        }
        let (message, _) = fixture
            .service
            .commit_message(&session.value.id, &request)
            .unwrap();
        assert_eq!(message.sequence, sequence);
    }

    let latest = fixture
        .service
        .list_messages(
            &session.value.id,
            &ListMessages {
                cursor: None,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(latest.snapshot_last_sequence, 5);
    assert_eq!(
        latest
            .items
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert_eq!(latest.items[1].usage.as_ref().unwrap().total_tokens, 15);
    let cursor = latest.next_cursor.unwrap();

    fixture
        .service
        .commit_message(&session.value.id, &text_message("message 6"))
        .unwrap();
    let older = fixture
        .service
        .list_messages(
            &session.value.id,
            &ListMessages {
                cursor: Some(cursor),
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(older.snapshot_last_sequence, 5);
    assert_eq!(
        older
            .items
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );

    let refreshed = fixture
        .service
        .list_messages(
            &session.value.id,
            &ListMessages {
                cursor: None,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(refreshed.snapshot_last_sequence, 6);
    assert_eq!(
        refreshed
            .items
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
}

#[test]
fn idempotency_persists_across_restart_and_cursor_keys_are_launch_scoped() {
    let fixture = Fixture::new();
    let created = fixture.create("Persistent", "create-persistent-session");
    fixture.create("Cursor boundary", "create-cursor-boundary-session");
    let page = fixture
        .service
        .list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: false,
            cursor: None,
            limit: 1,
        })
        .unwrap();
    let cursor = page
        .next_cursor
        .expect("two sessions with a one-item page must produce a cursor");
    let restarted = SessionService::new(fixture.home.path(), TOKEN);
    let replay = restarted
        .create_session(
            &CreateSession {
                profile_id: "default".to_owned(),
                title: Some("Persistent".to_owned()),
            },
            "create-persistent-session",
        )
        .unwrap();
    assert_eq!(replay, created);

    let other_launch = SessionService::new(
        fixture.home.path(),
        "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
    );
    assert!(matches!(
        other_launch.list_sessions(&ListSessions {
            profile_id: "default".to_owned(),
            query: None,
            archived: false,
            cursor: Some(cursor),
            limit: 10,
        }),
        Err(SessionError::InvalidCursor)
    ));
}
