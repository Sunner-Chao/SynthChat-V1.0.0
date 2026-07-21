use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{SESSION_SCHEMA_VERSION, SearchMode, SessionError};

const BASE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS app_meta (
    key TEXT PRIMARY KEY,
    integer_value INTEGER NOT NULL
);
INSERT OR IGNORE INTO app_meta(key, integer_value) VALUES('session_change_sequence', 0);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    title TEXT NOT NULL,
    preview TEXT NOT NULL,
    source TEXT NOT NULL,
    model TEXT NOT NULL,
    message_count INTEGER NOT NULL CHECK(message_count >= 0),
    archived INTEGER NOT NULL CHECK(archived IN (0, 1)),
    revision TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    next_message_sequence INTEGER NOT NULL CHECK(next_message_sequence >= 1),
    current_change INTEGER NOT NULL,
    persona_id TEXT CHECK(
        persona_id IS NULL OR (
            length(persona_id) = 40
            AND substr(persona_id, 1, 8) = 'persona_'
            AND substr(persona_id, 9) NOT GLOB '*[^0-9a-f]*'
        )
    )
);

CREATE TABLE IF NOT EXISTS session_versions (
    session_id TEXT NOT NULL,
    valid_from_change INTEGER NOT NULL,
    valid_to_change INTEGER,
    profile_id TEXT NOT NULL,
    title TEXT NOT NULL,
    preview TEXT NOT NULL,
    source TEXT NOT NULL,
    model TEXT NOT NULL,
    message_count INTEGER NOT NULL CHECK(message_count >= 0),
    archived INTEGER NOT NULL CHECK(archived IN (0, 1)),
    revision TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    persona_id TEXT CHECK(
        persona_id IS NULL OR (
            length(persona_id) = 40
            AND substr(persona_id, 1, 8) = 'persona_'
            AND substr(persona_id, 9) NOT GLOB '*[^0-9a-f]*'
        )
    ),
    PRIMARY KEY(session_id, valid_from_change),
    CHECK(valid_to_change IS NULL OR valid_to_change > valid_from_change)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_session_versions_current
    ON session_versions(session_id) WHERE valid_to_change IS NULL;
CREATE INDEX IF NOT EXISTS idx_session_versions_snapshot
    ON session_versions(profile_id, archived, valid_from_change, valid_to_change, updated_at DESC, session_id DESC);

CREATE TABLE IF NOT EXISTS messages (
    row_id INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK(sequence >= 1),
    role TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system', 'tool')),
    parts_json TEXT NOT NULL,
    reasoning TEXT,
    tool_calls_json TEXT NOT NULL,
    searchable_text TEXT NOT NULL,
    created_at TEXT NOT NULL,
    committed_change INTEGER NOT NULL,
    context_eligible INTEGER NOT NULL DEFAULT 1 CHECK(context_eligible IN (0, 1)),
    UNIQUE(session_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_messages_session_sequence
    ON messages(session_id, sequence DESC);
CREATE INDEX IF NOT EXISTS idx_messages_snapshot
    ON messages(session_id, committed_change, sequence DESC);

CREATE TABLE IF NOT EXISTS message_usage (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    prompt_tokens INTEGER NOT NULL CHECK(prompt_tokens >= 0),
    completion_tokens INTEGER NOT NULL CHECK(completion_tokens >= 0),
    total_tokens INTEGER NOT NULL CHECK(total_tokens >= 0),
    cost REAL
);

CREATE TABLE IF NOT EXISTS idempotency_records (
    method TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    response_json TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY(method, canonical_path, idempotency_key)
);

CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    path_digest TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(profile_id, path_digest)
);
CREATE INDEX IF NOT EXISTS idx_workspaces_profile_created
    ON workspaces(profile_id, created_at, id);

CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN (
        'queued', 'running', 'waitingApproval', 'waitingClarification',
        'cancelling', 'completed', 'cancelled', 'failed'
    )),
    last_sequence INTEGER NOT NULL DEFAULT 0 CHECK(last_sequence >= 0),
    user_message_id TEXT NOT NULL,
    message_id TEXT,
    queue_item_id TEXT UNIQUE,
    usage_json TEXT,
    error_json TEXT,
    pending_action_json TEXT,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    terminal_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_session_created
    ON runs(session_id, created_at DESC, id DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_runs_one_active_per_session
    ON runs(session_id) WHERE status IN (
        'running', 'waitingApproval', 'waitingClarification', 'cancelling'
    );

CREATE TABLE IF NOT EXISTS run_events (
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK(sequence >= 1),
    event_name TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    envelope_json TEXT NOT NULL,
    PRIMARY KEY(run_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_run_events_replay
    ON run_events(run_id, sequence);

CREATE TABLE IF NOT EXISTS run_turns (
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    turn_index INTEGER NOT NULL CHECK(turn_index >= 1),
    assistant_message_id TEXT NOT NULL CHECK(length(assistant_message_id) BETWEEN 1 AND 128),
    content TEXT,
    reasoning TEXT,
    finish_reason TEXT NOT NULL CHECK(finish_reason IN (
        'stop', 'toolCalls', 'length', 'contentFilter'
    )),
    usage_json TEXT NOT NULL CHECK(json_valid(usage_json) AND json_type(usage_json) = 'object'),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(run_id, turn_index)
);
CREATE INDEX IF NOT EXISTS idx_run_turns_replay
    ON run_turns(run_id, turn_index);

CREATE TABLE IF NOT EXISTS tool_invocations (
    run_id TEXT NOT NULL,
    turn_index INTEGER NOT NULL,
    call_index INTEGER NOT NULL CHECK(call_index >= 0),
    call_id TEXT NOT NULL CHECK(length(call_id) BETWEEN 1 AND 256),
    tool_name TEXT NOT NULL CHECK(length(tool_name) BETWEEN 1 AND 256),
    arguments_json TEXT NOT NULL
        CHECK(json_valid(arguments_json) AND json_type(arguments_json) = 'object'),
    status TEXT NOT NULL CHECK(status IN ('planned', 'running', 'completed', 'failed')),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
    checkpoint INTEGER NOT NULL DEFAULT 0 CHECK(checkpoint >= 0),
    result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
    error_json TEXT CHECK(error_json IS NULL OR json_valid(error_json)),
    provider_content TEXT,
    origin TEXT NOT NULL DEFAULT 'provider' CHECK(origin IN ('provider', 'codeRpc')),
    parent_call_id TEXT CHECK(
        parent_call_id IS NULL OR length(parent_call_id) BETWEEN 1 AND 256
    ),
    rpc_sequence INTEGER CHECK(rpc_sequence IS NULL OR rpc_sequence BETWEEN 1 AND 100),
    planned_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(run_id, call_id),
    UNIQUE(run_id, turn_index, call_index),
    FOREIGN KEY(run_id, turn_index)
        REFERENCES run_turns(run_id, turn_index) ON DELETE CASCADE,
    FOREIGN KEY(run_id, parent_call_id)
        REFERENCES tool_invocations(run_id, call_id) ON DELETE CASCADE,
    CHECK(
        (origin = 'provider' AND parent_call_id IS NULL AND rpc_sequence IS NULL)
        OR
        (origin = 'codeRpc' AND parent_call_id IS NOT NULL AND rpc_sequence IS NOT NULL)
    ),
    CHECK(
        (status = 'planned' AND attempt = 0 AND checkpoint = 0
            AND started_at IS NULL AND finished_at IS NULL
            AND result_json IS NULL AND error_json IS NULL AND provider_content IS NULL)
        OR
        (status = 'running' AND attempt >= 1 AND checkpoint >= 1
            AND started_at IS NOT NULL AND finished_at IS NULL
            AND result_json IS NULL AND error_json IS NULL AND provider_content IS NULL)
        OR
        (status = 'completed' AND attempt >= 1 AND checkpoint >= 2
            AND started_at IS NOT NULL AND finished_at IS NOT NULL
            AND result_json IS NOT NULL AND error_json IS NULL AND provider_content IS NOT NULL)
        OR
        (status = 'failed' AND attempt >= 1 AND checkpoint >= 2
            AND started_at IS NOT NULL AND finished_at IS NOT NULL
            AND result_json IS NULL AND error_json IS NOT NULL AND provider_content IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS idx_tool_invocations_turn
    ON tool_invocations(run_id, turn_index, call_index);
CREATE INDEX IF NOT EXISTS idx_tool_invocations_unfinished
    ON tool_invocations(run_id, status, turn_index, call_index)
    WHERE status IN ('planned', 'running');

CREATE TABLE IF NOT EXISTS hermes_import_batches (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    snapshot_fingerprint TEXT NOT NULL,
    reference_commit TEXT NOT NULL,
    source_schema_version INTEGER NOT NULL,
    source_session_count INTEGER NOT NULL CHECK(source_session_count >= 0),
    source_message_count INTEGER NOT NULL CHECK(source_message_count >= 0),
    source_model_usage_count INTEGER NOT NULL CHECK(source_model_usage_count >= 0),
    imported_session_count INTEGER NOT NULL CHECK(imported_session_count >= 0),
    imported_message_count INTEGER NOT NULL CHECK(imported_message_count >= 0),
    imported_model_usage_count INTEGER NOT NULL CHECK(imported_model_usage_count >= 0),
    omitted_attachment_count INTEGER NOT NULL CHECK(omitted_attachment_count >= 0),
    warnings_dropped INTEGER NOT NULL CHECK(warnings_dropped >= 0),
    result_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(profile_id, adapter_id, snapshot_fingerprint)
);

CREATE TABLE IF NOT EXISTS hermes_import_batch_warnings (
    batch_id TEXT NOT NULL REFERENCES hermes_import_batches(id) ON DELETE CASCADE,
    code TEXT NOT NULL,
    warning_count INTEGER NOT NULL CHECK(warning_count > 0),
    PRIMARY KEY(batch_id, code)
);

CREATE TABLE IF NOT EXISTS hermes_import_session_map (
    profile_id TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    source_key_digest TEXT NOT NULL,
    source_row_digest TEXT NOT NULL,
    parent_source_key_digest TEXT,
    target_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    target_revision TEXT NOT NULL,
    batch_id TEXT NOT NULL REFERENCES hermes_import_batches(id),
    PRIMARY KEY(profile_id, adapter_id, source_key_digest)
);

CREATE TABLE IF NOT EXISTS hermes_import_message_map (
    profile_id TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    source_key_digest TEXT NOT NULL,
    source_row_digest TEXT NOT NULL,
    source_session_key_digest TEXT NOT NULL,
    target_message_id TEXT REFERENCES messages(id) ON DELETE SET NULL,
    target_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    active INTEGER NOT NULL CHECK(active IN (0, 1)),
    compacted INTEGER NOT NULL CHECK(compacted IN (0, 1)),
    token_count INTEGER,
    finish_reason TEXT,
    batch_id TEXT NOT NULL REFERENCES hermes_import_batches(id),
    PRIMARY KEY(profile_id, adapter_id, source_key_digest)
);

CREATE TABLE IF NOT EXISTS hermes_import_model_usage (
    profile_id TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    source_key_digest TEXT NOT NULL,
    source_row_digest TEXT NOT NULL,
    target_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    route_fingerprint TEXT NOT NULL,
    model TEXT NOT NULL,
    billing_provider TEXT NOT NULL,
    billing_mode TEXT NOT NULL,
    billing_base_url_present INTEGER NOT NULL CHECK(billing_base_url_present IN (0, 1)),
    api_call_count INTEGER NOT NULL CHECK(api_call_count >= 0),
    input_tokens INTEGER NOT NULL CHECK(input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK(output_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK(cache_read_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL CHECK(cache_write_tokens >= 0),
    reasoning_tokens INTEGER NOT NULL CHECK(reasoning_tokens >= 0),
    estimated_cost_usd REAL NOT NULL,
    actual_cost_usd REAL NOT NULL,
    cost_status TEXT,
    cost_source TEXT,
    first_seen REAL,
    last_seen REAL,
    batch_id TEXT NOT NULL REFERENCES hermes_import_batches(id),
    PRIMARY KEY(profile_id, adapter_id, source_key_digest)
);
"#;

const MIGRATE_V1_TO_V2: &str = r#"
ALTER TABLE messages
    ADD COLUMN context_eligible INTEGER NOT NULL DEFAULT 1
    CHECK(context_eligible IN (0, 1));
"#;

const REPAIR_V2_RESPONSE_JSON: &str = r#"
ALTER TABLE idempotency_records
    ADD COLUMN response_json TEXT;
"#;

const MIGRATE_V4_TO_V5: &str = r#"
ALTER TABLE runs ADD COLUMN workspace_id TEXT REFERENCES workspaces(id) ON DELETE RESTRICT;
"#;

const APPROVAL_SCHEMA_V6: &str = r#"
CREATE TABLE IF NOT EXISTS run_approvals (
    approval_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    call_id TEXT NOT NULL,
    invocation_checkpoint INTEGER NOT NULL CHECK(invocation_checkpoint >= 1),
    tool_name TEXT NOT NULL CHECK(length(tool_name) BETWEEN 1 AND 256),
    input_summary TEXT CHECK(input_summary IS NULL OR length(input_summary) <= 2000),
    choices_json TEXT NOT NULL
        CHECK(json_valid(choices_json) AND json_type(choices_json) = 'array'),
    expires_at TEXT NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending', 'resolved')),
    decision TEXT CHECK(decision IS NULL OR decision IN ('once', 'deny')),
    reason TEXT CHECK(reason IS NULL OR length(reason) <= 2000),
    resolved_by TEXT CHECK(
        resolved_by IS NULL OR resolved_by IN ('user', 'expiry', 'cancellation')
    ),
    created_at TEXT NOT NULL,
    resolved_at TEXT,
    execution_claimed_at TEXT,
    UNIQUE(run_id, call_id),
    FOREIGN KEY(run_id, call_id)
        REFERENCES tool_invocations(run_id, call_id) ON DELETE CASCADE,
    CHECK(
        (state = 'pending' AND decision IS NULL AND reason IS NULL
            AND resolved_by IS NULL AND resolved_at IS NULL
            AND execution_claimed_at IS NULL)
        OR
        (state = 'resolved' AND decision IS NOT NULL
            AND resolved_by IS NOT NULL AND resolved_at IS NOT NULL
            AND (resolved_by = 'user' OR (decision = 'deny' AND reason IS NULL))
            AND (execution_claimed_at IS NULL
                OR (decision = 'once' AND resolved_by = 'user')))
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_run_approvals_one_pending
    ON run_approvals(run_id) WHERE state = 'pending';
CREATE INDEX IF NOT EXISTS idx_run_approvals_pending_expiry
    ON run_approvals(expires_at_unix_ms, run_id) WHERE state = 'pending';
"#;

const APPROVAL_BINDING_SCHEMA_V8: &str = r#"
CREATE TRIGGER IF NOT EXISTS run_approval_binding_is_complete
BEFORE INSERT ON run_approvals
WHEN NEW.profile_id IS NULL OR NEW.profile_id = ''
    OR NEW.session_id IS NULL OR NEW.session_id = ''
    OR NEW.arguments_sha256 IS NULL
    OR typeof(NEW.arguments_sha256) <> 'blob'
    OR length(NEW.arguments_sha256) <> 32
BEGIN
    SELECT RAISE(ABORT, 'tool approval execution binding is incomplete');
END;

CREATE TRIGGER IF NOT EXISTS run_approval_binding_is_immutable
BEFORE UPDATE OF run_id, profile_id, session_id, workspace_id, call_id,
    invocation_checkpoint, tool_name, arguments_sha256 ON run_approvals
WHEN NEW.run_id IS NOT OLD.run_id
    OR NEW.profile_id IS NOT OLD.profile_id
    OR NEW.session_id IS NOT OLD.session_id
    OR NEW.workspace_id IS NOT OLD.workspace_id
    OR NEW.call_id IS NOT OLD.call_id
    OR NEW.invocation_checkpoint IS NOT OLD.invocation_checkpoint
    OR NEW.tool_name IS NOT OLD.tool_name
    OR NEW.arguments_sha256 IS NOT OLD.arguments_sha256
BEGIN
    SELECT RAISE(ABORT, 'tool approval execution binding is immutable');
END;
"#;

const CLARIFICATION_SCHEMA_V9: &str = r#"
CREATE TABLE IF NOT EXISTS run_clarifications (
    request_id TEXT PRIMARY KEY CHECK(length(request_id) BETWEEN 1 AND 128),
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    call_id TEXT NOT NULL CHECK(length(call_id) BETWEEN 1 AND 256),
    invocation_checkpoint INTEGER NOT NULL CHECK(invocation_checkpoint >= 1),
    arguments_sha256 BLOB NOT NULL
        CHECK(typeof(arguments_sha256) = 'blob' AND length(arguments_sha256) = 32),
    question TEXT NOT NULL CHECK(length(question) BETWEEN 1 AND 2000),
    choices_json TEXT NOT NULL CHECK(
        json_valid(choices_json)
        AND json_type(choices_json) = 'array'
        AND json_array_length(choices_json) BETWEEN 0 AND 4
        AND (json_array_length(choices_json) < 1 OR (
            json_type(choices_json, '$[0]') = 'text'
            AND length(json_extract(choices_json, '$[0]')) BETWEEN 1 AND 500
        ))
        AND (json_array_length(choices_json) < 2 OR (
            json_type(choices_json, '$[1]') = 'text'
            AND length(json_extract(choices_json, '$[1]')) BETWEEN 1 AND 500
            AND json_extract(choices_json, '$[1]') <> json_extract(choices_json, '$[0]')
        ))
        AND (json_array_length(choices_json) < 3 OR (
            json_type(choices_json, '$[2]') = 'text'
            AND length(json_extract(choices_json, '$[2]')) BETWEEN 1 AND 500
            AND json_extract(choices_json, '$[2]') <> json_extract(choices_json, '$[0]')
            AND json_extract(choices_json, '$[2]') <> json_extract(choices_json, '$[1]')
        ))
        AND (json_array_length(choices_json) < 4 OR (
            json_type(choices_json, '$[3]') = 'text'
            AND length(json_extract(choices_json, '$[3]')) BETWEEN 1 AND 500
            AND json_extract(choices_json, '$[3]') <> json_extract(choices_json, '$[0]')
            AND json_extract(choices_json, '$[3]') <> json_extract(choices_json, '$[1]')
            AND json_extract(choices_json, '$[3]') <> json_extract(choices_json, '$[2]')
        ))
    ),
    state TEXT NOT NULL CHECK(state IN ('pending', 'resolved')),
    answer TEXT CHECK(answer IS NULL OR length(answer) BETWEEN 1 AND 10000),
    resolved_by TEXT CHECK(
        resolved_by IS NULL OR resolved_by IN ('user', 'cancellation', 'failure')
    ),
    created_at TEXT NOT NULL,
    resolved_at TEXT,
    continuation_claimed_at TEXT,
    UNIQUE(run_id, call_id),
    FOREIGN KEY(run_id, call_id)
        REFERENCES tool_invocations(run_id, call_id) ON DELETE CASCADE,
    CHECK(
        (state = 'pending' AND answer IS NULL AND resolved_by IS NULL
            AND resolved_at IS NULL AND continuation_claimed_at IS NULL)
        OR
        (state = 'resolved' AND resolved_by IS NOT NULL AND resolved_at IS NOT NULL
            AND ((resolved_by = 'user' AND answer IS NOT NULL)
                OR (resolved_by IN ('cancellation', 'failure') AND answer IS NULL))
            AND (continuation_claimed_at IS NULL OR resolved_by = 'user'))
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_run_clarifications_one_pending
    ON run_clarifications(run_id) WHERE state = 'pending';

CREATE TRIGGER IF NOT EXISTS run_clarification_request_is_immutable
BEFORE UPDATE OF request_id, run_id, call_id, invocation_checkpoint,
    arguments_sha256, question, choices_json ON run_clarifications
WHEN NEW.request_id IS NOT OLD.request_id
    OR NEW.run_id IS NOT OLD.run_id
    OR NEW.call_id IS NOT OLD.call_id
    OR NEW.invocation_checkpoint IS NOT OLD.invocation_checkpoint
    OR NEW.arguments_sha256 IS NOT OLD.arguments_sha256
    OR NEW.question IS NOT OLD.question
    OR NEW.choices_json IS NOT OLD.choices_json
BEGIN
    SELECT RAISE(ABORT, 'clarification request binding is immutable');
END;

CREATE TRIGGER IF NOT EXISTS run_clarification_resolution_is_immutable
BEFORE UPDATE OF state, answer, resolved_by, resolved_at ON run_clarifications
WHEN OLD.state = 'resolved' AND (
    NEW.state IS NOT OLD.state
    OR NEW.answer IS NOT OLD.answer
    OR NEW.resolved_by IS NOT OLD.resolved_by
    OR NEW.resolved_at IS NOT OLD.resolved_at
)
BEGIN
    SELECT RAISE(ABORT, 'clarification resolution is immutable');
END;

CREATE TRIGGER IF NOT EXISTS run_clarification_claim_is_single_use
BEFORE UPDATE OF continuation_claimed_at ON run_clarifications
WHEN OLD.continuation_claimed_at IS NOT NULL
    AND NEW.continuation_claimed_at IS NOT OLD.continuation_claimed_at
BEGIN
    SELECT RAISE(ABORT, 'clarification continuation claim is immutable');
END;
"#;

const CODE_RPC_SCHEMA_V10: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_invocations_code_rpc_sequence
    ON tool_invocations(run_id, parent_call_id, rpc_sequence)
    WHERE origin = 'codeRpc';

CREATE TRIGGER IF NOT EXISTS tool_invocation_code_rpc_is_complete
BEFORE INSERT ON tool_invocations
WHEN (NEW.origin = 'provider' AND (NEW.parent_call_id IS NOT NULL OR NEW.rpc_sequence IS NOT NULL))
    OR (NEW.origin = 'codeRpc' AND (NEW.parent_call_id IS NULL OR NEW.rpc_sequence IS NULL
        OR NEW.rpc_sequence < 1 OR NEW.rpc_sequence > 100))
    OR NEW.origin NOT IN ('provider', 'codeRpc')
BEGIN
    SELECT RAISE(ABORT, 'tool invocation origin binding is incomplete');
END;

CREATE TRIGGER IF NOT EXISTS tool_invocation_code_rpc_parent_is_valid
BEFORE INSERT ON tool_invocations
WHEN NEW.origin = 'codeRpc' AND NOT EXISTS(
    SELECT 1 FROM tool_invocations parent
    WHERE parent.run_id = NEW.run_id
        AND parent.call_id = NEW.parent_call_id
        AND parent.origin = 'provider'
        AND parent.tool_name = 'execute_code'
        AND parent.turn_index = NEW.turn_index
        AND parent.status = 'running'
)
BEGIN
    SELECT RAISE(ABORT, 'code RPC parent invocation is invalid');
END;

CREATE TRIGGER IF NOT EXISTS tool_invocation_origin_is_immutable
BEFORE UPDATE OF origin, parent_call_id, rpc_sequence ON tool_invocations
WHEN NEW.origin IS NOT OLD.origin
    OR NEW.parent_call_id IS NOT OLD.parent_call_id
    OR NEW.rpc_sequence IS NOT OLD.rpc_sequence
BEGIN
    SELECT RAISE(ABORT, 'tool invocation origin binding is immutable');
END;
"#;

const PROCESS_SCHEMA_V7: &str = r#"
CREATE TABLE IF NOT EXISTS terminal_processes (
    process_id TEXT PRIMARY KEY
        CHECK(length(process_id) = 40
            AND substr(process_id, 1, 8) = 'process_'
            AND substr(process_id, 9) NOT GLOB '*[^0-9a-f]*'),
    profile_id TEXT NOT NULL CHECK(length(profile_id) BETWEEN 1 AND 64),
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL CHECK(length(workspace_id) BETWEEN 1 AND 128),
    creator_run_id TEXT NOT NULL,
    call_id TEXT NOT NULL CHECK(length(call_id) BETWEEN 1 AND 256),
    command_preview TEXT NOT NULL CHECK(length(command_preview) BETWEEN 1 AND 2000),
    command_sha256 TEXT NOT NULL
        CHECK(length(command_sha256) = 64
            AND command_sha256 NOT GLOB '*[^0-9a-f]*'),
    pid INTEGER CHECK(pid IS NULL OR pid BETWEEN 1 AND 4294967295),
    process_identity TEXT CHECK(
        process_identity IS NULL OR length(process_identity) BETWEEN 1 AND 1024
    ),
    status TEXT NOT NULL CHECK(status IN (
        'starting', 'running', 'exited', 'killed', 'lost', 'failed_start'
    )),
    started_at TEXT NOT NULL CHECK(length(started_at) BETWEEN 1 AND 64),
    updated_at TEXT NOT NULL CHECK(length(updated_at) BETWEEN 1 AND 64),
    finished_at TEXT CHECK(finished_at IS NULL OR length(finished_at) BETWEEN 1 AND 64),
    exit_code INTEGER CHECK(exit_code IS NULL OR exit_code BETWEEN -2147483648 AND 2147483647),
    completion_reason TEXT CHECK(
        completion_reason IS NULL OR length(completion_reason) BETWEEN 1 AND 2000
    ),
    termination_source TEXT CHECK(
        termination_source IS NULL OR length(termination_source) BETWEEN 1 AND 128
    ),
    detached INTEGER NOT NULL CHECK(detached IN (0, 1)),
    completion_notification_required INTEGER NOT NULL
        CHECK(completion_notification_required IN (0, 1)),
    completion_notification_delivered INTEGER NOT NULL DEFAULT 0
        CHECK(completion_notification_delivered IN (0, 1)),
    UNIQUE(creator_run_id, call_id),
    FOREIGN KEY(creator_run_id, call_id)
        REFERENCES tool_invocations(run_id, call_id) ON DELETE CASCADE,
    CHECK(
        (status = 'starting' AND pid IS NULL AND process_identity IS NULL
            AND finished_at IS NULL AND exit_code IS NULL
            AND completion_reason IS NULL AND termination_source IS NULL)
        OR
        (status = 'running' AND pid IS NOT NULL
            AND finished_at IS NULL AND exit_code IS NULL
            AND completion_reason IS NULL AND termination_source IS NULL)
        OR
        (status = 'exited' AND pid IS NOT NULL AND finished_at IS NOT NULL
            AND exit_code IS NOT NULL AND completion_reason IS NOT NULL
            AND termination_source IS NOT NULL)
        OR
        (status = 'killed' AND pid IS NOT NULL AND finished_at IS NOT NULL
            AND completion_reason IS NOT NULL AND termination_source IS NOT NULL)
        OR
        (status = 'lost' AND finished_at IS NOT NULL
            AND exit_code IS NULL AND completion_reason IS NOT NULL
            AND termination_source IS NOT NULL)
        OR
        (status = 'failed_start' AND pid IS NULL AND process_identity IS NULL
            AND finished_at IS NOT NULL AND exit_code IS NULL
            AND completion_reason IS NOT NULL AND termination_source IS NOT NULL)
    ),
    CHECK(
        completion_notification_delivered = 0
        OR (completion_notification_required = 1
            AND status IN ('exited', 'killed', 'lost', 'failed_start'))
    )
);
CREATE INDEX IF NOT EXISTS idx_terminal_processes_owner
    ON terminal_processes(profile_id, session_id, started_at DESC, process_id DESC);
CREATE INDEX IF NOT EXISTS idx_terminal_processes_recovery
    ON terminal_processes(status, started_at, process_id)
    WHERE status IN ('starting', 'running');

CREATE TRIGGER IF NOT EXISTS terminal_process_status_is_irreversible
BEFORE UPDATE OF status ON terminal_processes
WHEN OLD.status IN ('exited', 'killed', 'lost', 'failed_start')
    AND NEW.status <> OLD.status
BEGIN
    SELECT RAISE(ABORT, 'terminal process status is immutable');
END;
"#;

const RUN_QUEUE_SCHEMA_V11: &str = r#"
CREATE TABLE IF NOT EXISTS run_queue (
    queue_item_id TEXT PRIMARY KEY
        CHECK(length(queue_item_id) = 38
            AND substr(queue_item_id, 1, 6) = 'queue_'
            AND substr(queue_item_id, 7) NOT GLOB '*[^0-9a-f]*'),
    run_id TEXT NOT NULL UNIQUE REFERENCES runs(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    profile_id TEXT NOT NULL CHECK(length(profile_id) BETWEEN 1 AND 64),
    request_json TEXT NOT NULL
        CHECK(json_valid(request_json) AND json_type(request_json) = 'object'),
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_run_queue_fifo
    ON run_queue(session_id, created_at, run_id);

CREATE TABLE IF NOT EXISTS runtime_leases (
    lease_name TEXT PRIMARY KEY CHECK(lease_name = 'run-runtime'),
    owner_id TEXT NOT NULL CHECK(length(owner_id) BETWEEN 1 AND 128),
    epoch INTEGER NOT NULL CHECK(epoch >= 1),
    expires_at_unix_ms INTEGER NOT NULL CHECK(expires_at_unix_ms >= 0),
    updated_at TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS run_queue_binding_is_valid
BEFORE INSERT ON run_queue
WHEN NOT EXISTS(
    SELECT 1 FROM runs
    WHERE id = NEW.run_id
        AND session_id = NEW.session_id
        AND profile_id = NEW.profile_id
        AND status = 'queued'
        AND queue_item_id = NEW.queue_item_id
)
BEGIN
    SELECT RAISE(ABORT, 'run queue binding is invalid');
END;

CREATE TRIGGER IF NOT EXISTS run_queue_binding_is_immutable
BEFORE UPDATE ON run_queue
BEGIN
    SELECT RAISE(ABORT, 'run queue binding is immutable');
END;
"#;

const ASYNC_TOOL_DELIVERY_SCHEMA_V12: &str = r#"
CREATE TABLE IF NOT EXISTS async_tool_deliveries (
    process_id TEXT PRIMARY KEY REFERENCES terminal_processes(process_id) ON DELETE CASCADE,
    delivery_kind TEXT NOT NULL CHECK(delivery_kind IN ('completion', 'watch')),
    watch_patterns_json TEXT NOT NULL
        CHECK(json_valid(watch_patterns_json) AND json_type(watch_patterns_json) = 'array'),
    state TEXT NOT NULL CHECK(state IN ('pending', 'delivered', 'dismissed')),
    settled_at TEXT CHECK(settled_at IS NULL OR length(settled_at) BETWEEN 1 AND 64),
    matched_pattern_count INTEGER CHECK(
        matched_pattern_count IS NULL OR matched_pattern_count BETWEEN 1 AND 16
    ),
    CHECK(
        (delivery_kind = 'completion' AND json_array_length(watch_patterns_json) = 0)
        OR (delivery_kind = 'watch' AND json_array_length(watch_patterns_json) BETWEEN 1 AND 16)
    ),
    CHECK(
        (state = 'pending' AND settled_at IS NULL AND matched_pattern_count IS NULL)
        OR (state = 'delivered' AND settled_at IS NOT NULL)
        OR (state = 'dismissed' AND settled_at IS NOT NULL AND matched_pattern_count IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS idx_async_tool_deliveries_pending
    ON async_tool_deliveries(state, process_id)
    WHERE state = 'pending';
"#;

const SESSION_PERSONA_COLUMNS_V13: [(&str, &str); 2] = [
    (
        "sessions",
        r#"ALTER TABLE sessions ADD COLUMN persona_id TEXT CHECK(
            persona_id IS NULL OR (
                length(persona_id) = 40
                AND substr(persona_id, 1, 8) = 'persona_'
                AND substr(persona_id, 9) NOT GLOB '*[^0-9a-f]*'
            )
        )"#,
    ),
    (
        "session_versions",
        r#"ALTER TABLE session_versions ADD COLUMN persona_id TEXT CHECK(
            persona_id IS NULL OR (
                length(persona_id) = 40
                AND substr(persona_id, 1, 8) = 'persona_'
                AND substr(persona_id, 9) NOT GLOB '*[^0-9a-f]*'
            )
        )"#,
    ),
];

const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
    searchable_text,
    message_id UNINDEXED,
    session_id UNINDEXED,
    tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS message_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO message_fts(searchable_text, message_id, session_id)
    VALUES(new.searchable_text, new.id, new.session_id);
END;
CREATE TRIGGER IF NOT EXISTS message_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM message_fts WHERE message_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS message_fts_update AFTER UPDATE OF searchable_text ON messages BEGIN
    DELETE FROM message_fts WHERE message_id = old.id;
    INSERT INTO message_fts(searchable_text, message_id, session_id)
    VALUES(new.searchable_text, new.id, new.session_id);
END;
"#;

pub(crate) fn initialize(hermes_home: &Path) -> Result<(PathBuf, SearchMode), SessionError> {
    ensure_safe_directory(hermes_home)?;
    let data_dir = hermes_home.join(".synthchat");
    ensure_safe_directory(&data_dir)?;
    let db_path = data_dir.join("sessions-v1.db");
    ensure_safe_file_if_present(&db_path)?;

    let mut connection = open(&db_path)?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(map_sqlite)?;
    if version > SESSION_SCHEMA_VERSION {
        return Err(SessionError::StorageUnavailable);
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)?;
    transaction.execute_batch(BASE_SCHEMA).map_err(map_sqlite)?;
    if version == 1 {
        transaction
            .execute_batch(MIGRATE_V1_TO_V2)
            .map_err(map_sqlite)?;
    }
    if !column_exists(&transaction, "idempotency_records", "response_json")? {
        transaction
            .execute_batch(REPAIR_V2_RESPONSE_JSON)
            .map_err(map_sqlite)?;
    }
    if !column_exists(&transaction, "runs", "workspace_id")? {
        transaction
            .execute_batch(MIGRATE_V4_TO_V5)
            .map_err(map_sqlite)?;
    }
    transaction
        .execute_batch(APPROVAL_SCHEMA_V6)
        .map_err(map_sqlite)?;
    migrate_approval_binding_v8(&transaction)?;
    transaction
        .execute_batch(PROCESS_SCHEMA_V7)
        .map_err(map_sqlite)?;
    transaction
        .execute_batch(CLARIFICATION_SCHEMA_V9)
        .map_err(map_sqlite)?;
    migrate_tool_invocation_origin_v10(&transaction)?;
    transaction
        .execute_batch(RUN_QUEUE_SCHEMA_V11)
        .map_err(map_sqlite)?;
    transaction
        .execute_batch(ASYNC_TOOL_DELIVERY_SCHEMA_V12)
        .map_err(map_sqlite)?;
    migrate_session_persona_v13(&transaction)?;
    if version < SESSION_SCHEMA_VERSION {
        transaction
            .pragma_update(None, "user_version", SESSION_SCHEMA_VERSION)
            .map_err(map_sqlite)?;
    }
    transaction.commit().map_err(map_sqlite)?;

    let search_mode = match initialize_fts(&mut connection) {
        Ok(()) => SearchMode::Fts5,
        Err(_) => SearchMode::Like,
    };
    Ok((db_path, search_mode))
}

fn migrate_session_persona_v13(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), SessionError> {
    for (table, statement) in SESSION_PERSONA_COLUMNS_V13 {
        if !column_exists(transaction, table, "persona_id")? {
            transaction.execute_batch(statement).map_err(map_sqlite)?;
        }
    }
    Ok(())
}

fn migrate_approval_binding_v8(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), SessionError> {
    let columns = [
        (
            "profile_id",
            "ALTER TABLE run_approvals ADD COLUMN profile_id TEXT",
        ),
        (
            "session_id",
            "ALTER TABLE run_approvals ADD COLUMN session_id TEXT",
        ),
        (
            "workspace_id",
            "ALTER TABLE run_approvals ADD COLUMN workspace_id TEXT",
        ),
        (
            "arguments_sha256",
            "ALTER TABLE run_approvals ADD COLUMN arguments_sha256 BLOB",
        ),
    ];
    let mut added_column = false;
    for (column, statement) in columns {
        if !column_exists(transaction, "run_approvals", column)? {
            transaction.execute_batch(statement).map_err(map_sqlite)?;
            added_column = true;
        }
    }

    let incomplete: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM run_approvals WHERE profile_id IS NULL \
                OR session_id IS NULL OR arguments_sha256 IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if added_column || incomplete > 0 {
        let rows = {
            let mut statement = transaction
                .prepare(
                    "SELECT approval.approval_id, run.profile_id, run.session_id, \
                        run.workspace_id, invocation.arguments_json \
                     FROM run_approvals approval \
                     JOIN runs run ON run.id = approval.run_id \
                     JOIN tool_invocations invocation \
                        ON invocation.run_id = approval.run_id \
                        AND invocation.call_id = approval.call_id",
                )
                .map_err(map_sqlite)?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(map_sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite)?
        };
        for (approval_id, profile_id, session_id, workspace_id, arguments_json) in rows {
            let arguments_sha256: [u8; 32] = Sha256::digest(arguments_json.as_bytes()).into();
            transaction
                .execute(
                    "UPDATE run_approvals SET profile_id = ?1, session_id = ?2, \
                        workspace_id = ?3, arguments_sha256 = ?4 WHERE approval_id = ?5",
                    params![
                        profile_id,
                        session_id,
                        workspace_id,
                        arguments_sha256.to_vec(),
                        approval_id,
                    ],
                )
                .map_err(map_sqlite)?;
        }
    }

    let invalid: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM run_approvals WHERE profile_id IS NULL \
                OR profile_id = '' OR session_id IS NULL OR session_id = '' \
                OR arguments_sha256 IS NULL OR typeof(arguments_sha256) <> 'blob' \
                OR length(arguments_sha256) <> 32)",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if invalid {
        return Err(SessionError::StorageUnavailable);
    }
    transaction
        .execute_batch(APPROVAL_BINDING_SCHEMA_V8)
        .map_err(map_sqlite)
}

fn migrate_tool_invocation_origin_v10(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), SessionError> {
    let columns = [
        (
            "origin",
            "ALTER TABLE tool_invocations ADD COLUMN origin TEXT NOT NULL DEFAULT 'provider' \
                CHECK(origin IN ('provider', 'codeRpc'))",
        ),
        (
            "parent_call_id",
            "ALTER TABLE tool_invocations ADD COLUMN parent_call_id TEXT \
                CHECK(parent_call_id IS NULL OR length(parent_call_id) BETWEEN 1 AND 256)",
        ),
        (
            "rpc_sequence",
            "ALTER TABLE tool_invocations ADD COLUMN rpc_sequence INTEGER \
                CHECK(rpc_sequence IS NULL OR rpc_sequence BETWEEN 1 AND 100)",
        ),
    ];
    for (column, statement) in columns {
        if !column_exists(transaction, "tool_invocations", column)? {
            transaction.execute_batch(statement).map_err(map_sqlite)?;
        }
    }
    transaction
        .execute_batch(CODE_RPC_SCHEMA_V10)
        .map_err(map_sqlite)
}

fn column_exists(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
) -> Result<bool, SessionError> {
    let mut statement = transaction
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(map_sqlite)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(map_sqlite)?;
    for row in rows {
        if row.map_err(map_sqlite)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn open(path: &Path) -> Result<Connection, SessionError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(map_sqlite)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(map_sqlite)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(map_sqlite)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(map_sqlite)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(map_sqlite)?;
    Ok(connection)
}

fn initialize_fts(connection: &mut Connection) -> Result<(), SessionError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)?;
    transaction.execute_batch(FTS_SCHEMA).map_err(map_sqlite)?;
    transaction
        .execute(
            "INSERT INTO message_fts(searchable_text, message_id, session_id) \
             SELECT searchable_text, id, session_id FROM messages \
             WHERE NOT EXISTS (SELECT 1 FROM message_fts WHERE message_id = messages.id)",
            [],
        )
        .map_err(map_sqlite)?;
    transaction.commit().map_err(map_sqlite)
}

fn ensure_safe_directory(path: &Path) -> Result<(), SessionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(SessionError::StorageUnavailable);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|_| SessionError::StorageUnavailable)?;
        }
        Err(_) => return Err(SessionError::StorageUnavailable),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| SessionError::StorageUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SessionError::StorageUnavailable);
    }
    Ok(())
}

fn ensure_safe_file_if_present(path: &Path) -> Result<(), SessionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(SessionError::StorageUnavailable)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SessionError::StorageUnavailable),
    }
}

pub(crate) fn map_sqlite(error: rusqlite::Error) -> SessionError {
    match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            SessionError::StorageBusy
        }
        _ => SessionError::StorageUnavailable,
    }
}
