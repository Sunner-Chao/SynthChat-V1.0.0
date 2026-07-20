CREATE TABLE schema_version (
    version INTEGER NOT NULL
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    model TEXT,
    parent_session_id TEXT,
    started_at REAL NOT NULL,
    ended_at REAL,
    message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_write_tokens INTEGER DEFAULT 0,
    reasoning_tokens INTEGER DEFAULT 0,
    estimated_cost_usd REAL,
    actual_cost_usd REAL,
    title TEXT,
    api_call_count INTEGER DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role TEXT NOT NULL,
    content TEXT,
    tool_call_id TEXT,
    tool_calls TEXT,
    tool_name TEXT,
    timestamp REAL NOT NULL,
    token_count INTEGER,
    finish_reason TEXT,
    reasoning TEXT,
    reasoning_content TEXT,
    reasoning_details TEXT,
    active INTEGER DEFAULT 1,
    compacted INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE session_model_usage (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    billing_provider TEXT NOT NULL DEFAULT '',
    billing_base_url TEXT NOT NULL DEFAULT '',
    billing_mode TEXT NOT NULL DEFAULT '',
    api_call_count INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd REAL NOT NULL DEFAULT 0,
    actual_cost_usd REAL NOT NULL DEFAULT 0,
    cost_status TEXT,
    cost_source TEXT,
    first_seen REAL,
    last_seen REAL,
    PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode)
);

INSERT INTO schema_version (version) VALUES (21);

INSERT INTO sessions (
    id, source, model, parent_session_id, started_at, ended_at, message_count,
    tool_call_count, input_tokens, output_tokens, cache_read_tokens,
    cache_write_tokens, reasoning_tokens, estimated_cost_usd, actual_cost_usd,
    title, api_call_count, archived
) VALUES (
    'synthetic-session-1', 'cli', 'synthetic/model', NULL, 1000.0, 1010.0, 6,
    2, 120, 45, 10, 4, 7, 0.0125, 0.011, 'Synthetic fixture', 3, 0
);

INSERT INTO messages (
    id, session_id, role, content, tool_call_id, tool_calls, tool_name,
    timestamp, token_count, finish_reason, reasoning, reasoning_content,
    reasoning_details, active, compacted
) VALUES (
    10, 'synthetic-session-1', 'assistant', 'Details answer', NULL, NULL, NULL,
    2.0, 8, 'stop', NULL, NULL,
    '[{"type":"summary","text":"First detail"},{"thinking":"Second detail"},{"encrypted_content":"synthetic"}]',
    NULL, 0
);

INSERT INTO messages (
    id, session_id, role, content, tool_call_id, tool_calls, tool_name,
    timestamp, token_count, finish_reason, reasoning, reasoning_content,
    reasoning_details, active, compacted
) VALUES (
    11, 'synthetic-session-1', 'assistant', 'Direct answer', NULL, NULL, NULL,
    1.0, 5, 'stop', 'Direct reasoning', 'Legacy reasoning',
    '[{"text":"Details reasoning"}]', 1, 0
);

INSERT INTO messages (
    id, session_id, role, content, tool_call_id, tool_calls, tool_name,
    timestamp, token_count, finish_reason, reasoning, reasoning_content,
    reasoning_details, active, compacted
) VALUES (
    12, 'synthetic-session-1', 'assistant',
    char(0) || 'json:[{"type":"text","text":"Multimodal text"},{"type":"image_url","image_url":{"url":"data:image/png;base64,U1lOVEhFVElD"}},{"type":"input_image","image_url":"C:\\synthetic\\fixture.png"},{"type":"image_url","image_url":"https://assets.example.test/synthetic.png"}]',
    NULL,
    '[{"id":"call-1","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"echo synthetic\"}"}},{"id":"ignored","function":{"name":"","arguments":"{}"}},42]',
    NULL, 1.0, 6, 'tool_calls', NULL, 'Legacy reasoning',
    '[{"text":"Details fallback"}]', 0, 1
);

INSERT INTO messages (
    id, session_id, role, content, tool_call_id, tool_calls, tool_name,
    timestamp, token_count, finish_reason, reasoning, reasoning_content,
    reasoning_details, active, compacted
) VALUES (
    13, 'synthetic-session-1', 'user', 'Rewound and not compacted', NULL, NULL, NULL,
    0.5, 4, NULL, NULL, NULL, NULL, 0, 0
);

INSERT INTO messages (
    id, session_id, role, content, tool_call_id, tool_calls, tool_name,
    timestamp, token_count, finish_reason, reasoning, reasoning_content,
    reasoning_details, active, compacted
) VALUES (
    14, 'synthetic-session-1', 'assistant', char(0) || 'json:{bad', NULL,
    '{bad', NULL, 3.0, NULL, NULL, NULL, NULL, '{bad', 1, 0
);

INSERT INTO messages (
    id, session_id, role, content, tool_call_id, tool_calls, tool_name,
    timestamp, token_count, finish_reason, reasoning, reasoning_content,
    reasoning_details, active, compacted
) VALUES (
    15, 'synthetic-session-1', 'user', 'User content', NULL,
    '[{"call_id":"call-2","function":{"name":"search","arguments":"not-json"}}]',
    NULL, 4.0, NULL, NULL, 'Must not surface', NULL, NULL, 1, 0
);

INSERT INTO session_model_usage (
    session_id, model, billing_provider, billing_base_url, billing_mode,
    api_call_count, input_tokens, output_tokens, cache_read_tokens,
    cache_write_tokens, reasoning_tokens, estimated_cost_usd, actual_cost_usd,
    cost_status, cost_source, first_seen, last_seen
) VALUES (
    'synthetic-session-1', 'synthetic/model', 'synthetic-provider',
    'https://private-route.example.test/v1?token=synthetic', 'api-key',
    3, 120, 45, 10, 4, 7, 0.0125, 0.011,
    'estimated', 'synthetic-catalog', 1000.0, 1009.0
);
