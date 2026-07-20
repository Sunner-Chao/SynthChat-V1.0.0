use std::{
    collections::HashMap,
    convert::Infallible,
    future::pending,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_stream::stream;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, Method, Request, Response, StatusCode, header},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_PATH: &str = "generated/approved.txt";
const WRITE_CONTENT: &str = "SECRET_WRITE_BODY_DO_NOT_EXPOSE\n";
const PATCH_REPLACE_CALL_ID: &str = "call-patch-replace-1";
const PATCH_REPLACE_PATH: &str = "src/replace.txt";
const PATCH_REPLACE_OLD: &str = "PATCH_OLD_SECRET_DO_NOT_EXPOSE";
const PATCH_REPLACE_NEW: &str = "PATCH_NEW_SECRET_DO_NOT_EXPOSE";
const PATCH_V4A_CALL_ID: &str = "call-patch-v4a-1";
const PATCH_V4A_PATH: &str = "generated/v4a.txt";
const PATCH_V4A_CONTENT: &str = "PATCH_V4A_SECRET_DO_NOT_EXPOSE";
const PATCH_V4A_TEXT: &str = "*** Begin Patch\n*** Add File: generated/v4a.txt\n+PATCH_V4A_SECRET_DO_NOT_EXPOSE\n*** End Patch";
const TERMINAL_CALL_ID: &str = "call-terminal-foreground-1";
const TERMINAL_PATH: &str = "generated/terminal-approved.txt";
const TERMINAL_FILE_CONTENT: &str = "TERMINAL_FILE_SECRET_DO_NOT_EXPOSE\n";
const TERMINAL_OUTPUT: &str = "TERMINAL_OUTPUT_SECRET_DO_NOT_EXPOSE";
const BACKGROUND_TERMINAL_CALL_ID: &str = "call-terminal-background-1";
const BACKGROUND_POLL_CALL_ID: &str = "call-process-poll-1";
const BACKGROUND_KILL_CALL_ID: &str = "call-process-kill-1";
const BACKGROUND_OUTPUT: &str = "BACKGROUND_OUTPUT_SECRET_DO_NOT_EXPOSE";
const ASYNC_COMPLETION_CALL_ID: &str = "call-async-completion-1";
const ASYNC_WATCH_CALL_ID: &str = "call-async-watch-1";
const ASYNC_CANCEL_CALL_ID: &str = "call-async-cancel-1";
const ASYNC_CANCEL_KILL_CALL_ID: &str = "call-async-cancel-kill-1";
const ASYNC_PRIVATE_OUTPUT: &str = "ASYNC_PRIVATE_OUTPUT_DO_NOT_EXPOSE";
const ASYNC_WATCH_PATTERN: &str = "ASYNC_WATCH_READY_DO_NOT_EXPOSE";
const CLARIFICATION_CALL_ID: &str = "call-clarification-1";
const CLARIFICATION_QUESTION: &str = "Which deployment target should I use?";
const CLARIFICATION_PRIVATE_ANSWER: &str = "  PRIVATE_CLARIFICATION_ANSWER_DO_NOT_EXPOSE\n  ";
const MEMORY_CALL_ID: &str = "call-memory-1";
const INITIAL_MEMORY_CONTENT: &str = "INITIAL_MEMORY_SNAPSHOT_MARKER";
const MEMORY_WRITE_CONTENT: &str = "PRIVATE_MEMORY_WRITE_DO_NOT_EXPOSE";
const CODE_CALL_ID: &str = "call-execute-code-1";
const CODE_OUTPUT: &str = "PRIVATE_CODE_STDOUT_DO_NOT_EXPOSE";
const CODE_INPUT_CONTENT: &str = "PRIVATE_CODE_INPUT_DO_NOT_EXPOSE";
const CODE_FILE_PATH: &str = "generated/code-approved.txt";
const CODE_FILE_CONTENT: &str = "PRIVATE_CODE_FILE_DO_NOT_EXPOSE";
const CODE_PROFILE_SECRET: &str = "PRIVATE_CODE_PROFILE_SECRET_DO_NOT_EXPOSE";
const CODE_ENV_ABSENT: &str = "CODE_ENVIRONMENT_SECRET_ABSENT";
const CODE_HEARTBEAT_PATH: &str = "generated/code-heartbeat.txt";
const MCP_CALL_ID: &str = "call-mcp-echo-1";
const MCP_TOOL_NAME: &str = "mcp__fixture__echo";
const MCP_PRIVATE_RESULT: &str = "MCP_PRIVATE_RESULT_DO_NOT_EXPOSE";
const MCP_REMOTE_SECRET: &str = "MCP_REMOTE_BEARER_DO_NOT_EXPOSE";
const MCP_REMOTE_SECRET_NAME: &str = "MCP_REMOTE_TOKEN";
const BROWSER_DOWNLOAD_CALL_ID: &str = "call-browser-download-1";

struct Harness {
    app: Router,
    _home: TempDir,
    provider: Arc<MockProvider>,
    provider_server: JoinHandle<()>,
}

impl Harness {
    async fn new() -> Self {
        let provider = Arc::new(MockProvider::default());
        let provider_app = Router::new()
            .route("/v1/chat/completions", post(mock_chat_completions))
            .with_state(provider.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let provider_server = tokio::spawn(async move {
            axum::serve(listener, provider_app).await.unwrap();
        });

        let home = tempfile::tempdir().unwrap();
        let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
        let app = build_router(AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        ));
        let harness = Self {
            app,
            _home: home,
            provider,
            provider_server,
        };
        harness
            .configure_provider(&format!("http://{address}/v1"))
            .await;
        harness
    }

    async fn configure_provider(&self, base_url: &str) {
        let current = authorized_request(
            &self.app,
            Method::GET,
            "/api/v1/profiles/default/config",
            Body::empty(),
        )
        .await;
        assert_eq!(current.status(), StatusCode::OK);
        let etag = current.headers()[header::ETAG].to_str().unwrap().to_owned();
        let _ = json_body(current).await;

        let patch = authorized_request_builder(
            &self.app,
            Request::patch("/api/v1/profiles/default/config")
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, etag)
                .body(Body::from(
                    json!({
                        "model": {
                            "provider": "lmstudio",
                            "model": "test-model",
                            "baseUrl": base_url
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(patch.status(), StatusCode::OK);
        let configured = json_body(patch).await;
        assert_eq!(configured["model"]["provider"], "lmstudio");
        assert_eq!(configured["model"]["baseUrl"], base_url);
    }

    async fn enable_session_search(&self) {
        self.enable_toolset("session_search").await;
    }

    async fn enable_toolset(&self, toolset_id: &str) {
        let current = authorized_request(
            &self.app,
            Method::GET,
            "/api/v1/profiles/default/config",
            Body::empty(),
        )
        .await;
        assert_eq!(current.status(), StatusCode::OK);
        let etag = current.headers()[header::ETAG].to_str().unwrap().to_owned();
        let _ = json_body(current).await;
        let updated = authorized_request_builder(
            &self.app,
            Request::patch("/api/v1/profiles/default/config")
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, etag)
                .body(Body::from(
                    json!({"toolsets": {(toolset_id): true}}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(updated.status(), StatusCode::OK);
        assert_eq!(json_body(updated).await["toolsets"][toolset_id], true);
    }

    fn install_skill(&self) {
        let directory = self._home.path().join("skills/research/papers");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: paper-search\ndescription: Search papers with citations\n---\n# Paper search\n",
        )
        .unwrap();
    }

    fn restarted_app(&self) -> Router {
        let connection =
            rusqlite::Connection::open(self._home.path().join(".synthchat/sessions-v1.db"))
                .unwrap();
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
        self.contending_app()
    }

    fn contending_app(&self) -> Router {
        let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let profiles = ProfileService::with_credential_store(self._home.path().to_owned(), store);
        build_router(AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        ))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.provider_server.abort();
    }
}

#[derive(Default)]
struct MockProvider {
    calls: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    request_seen: Notify,
}

impl MockProvider {
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    async fn wait_for_calls(&self, expected: usize) {
        timeout(WAIT_TIMEOUT, async {
            loop {
                if self.call_count() >= expected {
                    return;
                }
                self.request_seen.notified().await;
            }
        })
        .await
        .expect("the mock provider should receive the request");
    }
}

#[derive(Default)]
struct StreamableRunFixture {
    methods: Mutex<Vec<String>>,
}

async fn streamable_mcp_post(
    State(fixture): State<Arc<StreamableRunFixture>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response<Body> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer MCP_REMOTE_BEARER_DO_NOT_EXPOSE")
    {
        return remote_fixture_response(StatusCode::UNAUTHORIZED, "missing bearer");
    }
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    fixture.methods.lock().unwrap().push(method.to_owned());
    let initial = method == "initialize";
    if initial
        && (headers.contains_key("mcp-session-id") || headers.contains_key("mcp-protocol-version"))
    {
        return remote_fixture_response(StatusCode::BAD_REQUEST, "unexpected initial headers");
    }
    if !initial
        && (headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some("run-fixture-session")
            || headers
                .get("mcp-protocol-version")
                .and_then(|value| value.to_str().ok())
                != Some("2025-06-18"))
    {
        return remote_fixture_response(StatusCode::BAD_REQUEST, "missing session headers");
    }
    let id = request.get("id").cloned();
    match method {
        "initialize" => remote_fixture_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"protocolVersion": "2025-06-18", "capabilities": {}}
        })),
        "notifications/initialized" => remote_fixture_empty(StatusCode::ACCEPTED),
        "tools/list" => remote_fixture_sse(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [{
                "name": "echo",
                "description": "Echo a value",
                "inputSchema": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                    "additionalProperties": false
                }
            }]}
        })),
        "tools/call" => remote_fixture_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": MCP_PRIVATE_RESULT}],
                "isError": false
            }
        })),
        _ => remote_fixture_response(StatusCode::BAD_REQUEST, "unknown method"),
    }
}

async fn streamable_mcp_delete(headers: HeaderMap) -> Response<Body> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer MCP_REMOTE_BEARER_DO_NOT_EXPOSE")
        || headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some("run-fixture-session")
        || headers
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            != Some("2025-06-18")
    {
        return remote_fixture_response(StatusCode::BAD_REQUEST, "missing close headers");
    }
    remote_fixture_empty(StatusCode::NO_CONTENT)
}

fn remote_fixture_json(value: Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header("mcp-session-id", "run-fixture-session")
        .body(Body::from(value.to_string()))
        .unwrap()
}

fn remote_fixture_sse(value: Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header("mcp-session-id", "run-fixture-session")
        .body(Body::from(format!("event: message\ndata: {value}\n\n")))
        .unwrap()
}

fn remote_fixture_empty(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("mcp-session-id", "run-fixture-session")
        .body(Body::empty())
        .unwrap()
}

fn remote_fixture_response(status: StatusCode, detail: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(detail.to_owned()))
        .unwrap()
}

struct LegacySseRunFixture {
    channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>>,
    next_session: AtomicUsize,
}

async fn legacy_mcp_sse_get(
    State(fixture): State<Arc<LegacySseRunFixture>>,
    headers: HeaderMap,
) -> Response<Body> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer MCP_REMOTE_BEARER_DO_NOT_EXPOSE")
    {
        return remote_fixture_response(StatusCode::UNAUTHORIZED, "missing bearer");
    }
    let session = format!(
        "legacy-run-{}",
        fixture.next_session.fetch_add(1, Ordering::Relaxed)
    );
    let (sender, mut receiver) = mpsc::unbounded_channel();
    fixture
        .channels
        .lock()
        .unwrap()
        .insert(session.clone(), sender);
    let initial = format!("event: endpoint\ndata: /messages?session={session}\n\n");
    let body = Body::from_stream(stream! {
        yield Ok::<_, Infallible>(Bytes::from(initial));
        while let Some(event) = receiver.recv().await {
            yield Ok::<_, Infallible>(Bytes::from(event));
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

async fn legacy_mcp_sse_post(
    State(fixture): State<Arc<LegacySseRunFixture>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response<Body> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer MCP_REMOTE_BEARER_DO_NOT_EXPOSE")
    {
        return remote_fixture_response(StatusCode::UNAUTHORIZED, "missing bearer");
    }
    let Some(session) = query.get("session") else {
        return remote_fixture_response(StatusCode::BAD_REQUEST, "missing endpoint session");
    };
    let Some(sender) = fixture.channels.lock().unwrap().get(session).cloned() else {
        return remote_fixture_response(StatusCode::NOT_FOUND, "unknown endpoint session");
    };
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => Some(json!({"protocolVersion": "2024-11-05", "capabilities": {}})),
        "tools/list" => Some(json!({"tools": [{
            "name": "echo",
            "description": "Echo a value",
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }
        }]})),
        "tools/call" => Some(json!({
            "content": [{"type": "text", "text": MCP_PRIVATE_RESULT}],
            "isError": false
        })),
        "notifications/initialized" => None,
        _ => return remote_fixture_response(StatusCode::BAD_REQUEST, "unknown method"),
    };
    if let (Some(id), Some(result)) = (request.get("id").cloned(), result) {
        let message = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if sender
            .send(format!("event: message\ndata: {message}\n\n"))
            .is_err()
        {
            return remote_fixture_response(StatusCode::CONFLICT, "closed SSE stream");
        }
    }
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .body(Body::empty())
        .unwrap()
}

fn tool_call_payload(call_id: &str, name: &str, arguments: String) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })
}

async fn mock_chat_completions(
    State(provider): State<Arc<MockProvider>>,
    Json(request): Json<Value>,
) -> Response<Body> {
    let messages = request["messages"].as_array();
    let last = messages.and_then(|messages| messages.last());
    let slow = last
        .and_then(|message| message["content"].as_str())
        .is_some_and(|content| content.contains("slow response"));
    let has_tool_result = last
        .and_then(|message| message["role"].as_str())
        .is_some_and(|role| role == "tool");
    let last_tool_call_id = last
        .and_then(|message| message["tool_call_id"].as_str())
        .map(ToOwned::to_owned);
    let has_skill_result = last
        .and_then(|message| message["content"].as_str())
        .is_some_and(|content| content.contains("paper-search"));
    let wants_session_search = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use session search"));
    let wants_skills_list = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use skills list"));
    let wants_read_file = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use workspace file"));
    let wants_write_file = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use workspace write"));
    let wants_patch_replace = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use workspace patch replace"));
    let wants_patch_v4a = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use workspace patch v4a"));
    let wants_terminal_foreground = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use terminal foreground"));
    let wants_process_lifecycle = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use terminal process lifecycle"));
    let wants_async_completion = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use async completion delivery"));
    let wants_async_restart = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("async delivery restart recovery"));
    let wants_async_watch = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use async watch delivery"));
    let wants_async_cancel = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use async delivery cancellation"));
    let wants_async_invalid = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use invalid async delivery"));
    let wants_clarification = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use clarification"));
    let wants_clarification_choice = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use clarification choice"));
    let wants_memory_write = messages
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .and_then(|message| message["content"].as_str())
        .is_some_and(|content| content.contains("use memory write"));
    let wants_execute_code = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use execute code"));
    let wants_execute_code_slow = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use execute code slow"));
    let wants_mcp_echo = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use mcp echo"));
    let wants_browser_download = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .any(|content| content.contains("use browser download approval"));
    let lifecycle_process_id = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .find(|message| message["tool_call_id"] == BACKGROUND_TERMINAL_CALL_ID)
        .and_then(|message| message["content"].as_str())
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .and_then(|content| content["session_id"].as_str().map(ToOwned::to_owned));
    let async_process_id = messages
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .find(|message| {
            matches!(
                message["tool_call_id"].as_str(),
                Some(ASYNC_COMPLETION_CALL_ID | ASYNC_WATCH_CALL_ID | ASYNC_CANCEL_CALL_ID)
            )
        })
        .and_then(|message| message["content"].as_str())
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .and_then(|content| content["session_id"].as_str().map(ToOwned::to_owned));
    provider.requests.lock().unwrap().push(request);
    provider.calls.fetch_add(1, Ordering::SeqCst);
    provider.request_seen.notify_one();

    let body = Body::from_stream(stream! {
        if slow {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Partial\"}}]}\n\n",
            ));
            pending::<()>().await;
        } else if wants_browser_download && !has_tool_result {
            let payload = tool_call_payload(
                BROWSER_DOWNLOAD_CALL_ID,
                "browser_download",
                json!({"selector": "a#report", "snapshotId": "snapshot_abc123"}).to_string(),
            );
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_browser_download && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Browser download decision handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_mcp_echo && !has_tool_result {
            let payload = tool_call_payload(
                MCP_CALL_ID,
                MCP_TOOL_NAME,
                json!({"text": "hello"}).to_string(),
            );
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_mcp_echo && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"MCP handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_execute_code && !has_tool_result {
            let code = if wants_execute_code_slow {
                format!(
                    "import time\nfrom pathlib import Path\npath = Path('{CODE_HEARTBEAT_PATH}')\npath.parent.mkdir(exist_ok=True)\nwhile True:\n    with path.open('a', encoding='utf-8') as output:\n        output.write('x')\n        output.flush()\n    time.sleep(0.05)\n"
                )
            } else {
                format!(
                    "import os\nfrom pathlib import Path\nfrom hermes_tools import read_file\nread_file('input.txt')\nPath('generated').mkdir(exist_ok=True)\nPath('{CODE_FILE_PATH}').write_text('{CODE_FILE_CONTENT}', encoding='utf-8')\nprint('{CODE_OUTPUT}')\nprint(os.environ.get('OPENAI_API_KEY', '{CODE_ENV_ABSENT}'))\n"
                )
            };
            let payload = tool_call_payload(
                CODE_CALL_ID,
                "execute_code",
                json!({"code": code}).to_string(),
            );
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_execute_code && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Code handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_memory_write && !has_tool_result {
            let arguments = json!({
                "action": "add",
                "target": "memory",
                "content": MEMORY_WRITE_CONTENT,
            })
            .to_string();
            let payload = tool_call_payload(MEMORY_CALL_ID, "memory", arguments);
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_memory_write && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Memory handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_clarification && !has_tool_result {
            let arguments = if wants_clarification_choice {
                json!({
                    "question": CLARIFICATION_QUESTION,
                    "choices": ["staging", "production"],
                })
            } else {
                json!({"question": CLARIFICATION_QUESTION})
            }
            .to_string();
            let payload = tool_call_payload(CLARIFICATION_CALL_ID, "clarify", arguments);
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_clarification && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Clarification handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_async_completion && !has_tool_result {
            let arguments = json!({
                "command": if wants_async_restart {
                    "sleep 7".to_owned()
                } else {
                    format!("printf '{ASYNC_PRIVATE_OUTPUT}\\n'")
                },
                "background": true,
                "notify_on_complete": true,
            })
            .to_string();
            let payload = tool_call_payload(ASYNC_COMPLETION_CALL_ID, "terminal", arguments);
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_async_completion && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Async completion handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_async_watch && !has_tool_result {
            let arguments = json!({
                "command": format!(
                    "printf '{} {}\\n'; sleep 1",
                    ASYNC_PRIVATE_OUTPUT,
                    ASYNC_WATCH_PATTERN,
                ),
                "background": true,
                "watch_patterns": [ASYNC_WATCH_PATTERN],
            })
            .to_string();
            let payload = tool_call_payload(ASYNC_WATCH_CALL_ID, "terminal", arguments);
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_async_watch && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Async watch handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_async_cancel {
            match last_tool_call_id.as_deref() {
                None => {
                    let arguments = json!({
                        "command": "while true; do sleep 1; done",
                        "background": true,
                        "notify_on_complete": true,
                    })
                    .to_string();
                    let payload = tool_call_payload(ASYNC_CANCEL_CALL_ID, "terminal", arguments);
                    yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
                }
                Some(ASYNC_CANCEL_CALL_ID) => {
                    let arguments = json!({
                        "action": "kill",
                        "session_id": async_process_id.as_deref().unwrap_or("process_missing"),
                    })
                    .to_string();
                    let payload = tool_call_payload(ASYNC_CANCEL_KILL_CALL_ID, "process", arguments);
                    yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
                }
                Some(ASYNC_CANCEL_KILL_CALL_ID) => {
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Async cancellation handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
                    ));
                }
                Some(_) => {
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Unexpected async cancellation state\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                    ));
                }
            }
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_async_invalid && !has_tool_result {
            let arguments = json!({
                "command": "printf invalid",
                "notify_on_complete": true,
            })
            .to_string();
            let payload = tool_call_payload("call-async-invalid-1", "terminal", arguments);
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_process_lifecycle {
            match last_tool_call_id.as_deref() {
                None => {
                    let command = format!(
                        "while true; do printf '{BACKGROUND_OUTPUT}\\n'; sleep 5; done"
                    );
                    let arguments = json!({"command": command, "background": true}).to_string();
                    let payload = json!({
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "id": BACKGROUND_TERMINAL_CALL_ID,
                                    "type": "function",
                                    "function": {"name": "terminal", "arguments": arguments}
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
                    });
                    yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
                }
                Some(BACKGROUND_TERMINAL_CALL_ID) => {
                    let arguments = json!({
                        "action": "poll",
                        "session_id": lifecycle_process_id.as_deref().unwrap_or("process_missing"),
                    }).to_string();
                    let payload = tool_call_payload(BACKGROUND_POLL_CALL_ID, "process", arguments);
                    yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
                }
                Some(BACKGROUND_POLL_CALL_ID) => {
                    let arguments = json!({
                        "action": "kill",
                        "session_id": lifecycle_process_id.as_deref().unwrap_or("process_missing"),
                    }).to_string();
                    let payload = tool_call_payload(BACKGROUND_KILL_CALL_ID, "process", arguments);
                    yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
                }
                Some(BACKGROUND_KILL_CALL_ID) => {
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Process lifecycle handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
                    ));
                }
                Some(_) => {
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Unexpected lifecycle state\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                    ));
                }
            }
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_terminal_foreground && !has_tool_result {
            let command = format!(
                "mkdir -p generated && printf '{}\\n' && printf '{}' > {}",
                TERMINAL_OUTPUT,
                TERMINAL_FILE_CONTENT.trim_end(),
                TERMINAL_PATH,
            );
            let arguments = json!({"command": command}).to_string();
            let payload = json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": TERMINAL_CALL_ID,
                            "type": "function",
                            "function": {"name": "terminal", "arguments": arguments}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
            });
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if (wants_patch_replace || wants_patch_v4a) && !has_tool_result {
            let (call_id, arguments) = if wants_patch_v4a {
                (
                    PATCH_V4A_CALL_ID,
                    json!({"mode": "patch", "patch": PATCH_V4A_TEXT}).to_string(),
                )
            } else {
                (
                    PATCH_REPLACE_CALL_ID,
                    json!({
                        "mode": "replace",
                        "path": PATCH_REPLACE_PATH,
                        "old_string": PATCH_REPLACE_OLD,
                        "new_string": PATCH_REPLACE_NEW,
                        "replace_all": false,
                    })
                    .to_string(),
                )
            };
            let payload = json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": call_id,
                            "type": "function",
                            "function": {"name": "patch", "arguments": arguments}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
            });
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_write_file && !has_tool_result {
            let arguments = json!({"path": WRITE_PATH, "content": WRITE_CONTENT}).to_string();
            let payload = json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-write-file-1",
                            "type": "function",
                            "function": {"name": "write_file", "arguments": arguments}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
            });
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_read_file && !has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-read-file-1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_skills_list && !has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-skills-1\",\"type\":\"function\",\"function\":{\"name\":\"skills_list\",\"arguments\":\"{\\\"limit\\\":10}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_session_search && !has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-search-1\",\"type\":\"function\",\"function\":{\"name\":\"session_search\",\"arguments\":\"{\\\"query\\\":\\\"needle\\\",\\\"limit\\\":5}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_terminal_foreground && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Terminal handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if (wants_patch_replace || wants_patch_v4a) && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Patch handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_write_file && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Write handled\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if wants_read_file && has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Read workspace\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if has_skill_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Found skills\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else if has_tool_result {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Found history\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        } else {
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Think carefully. \"}}]}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2,\"total_tokens\":9,\"cost\":0.01}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn completed_run_streams_replays_and_survives_session_deletion() {
    let harness = Harness::new().await;
    let session_id = create_session(&harness.app, "completed-run-session").await;
    let initial_request = run_request("request-0001", "hello");

    let accepted = post_run(
        &harness.app,
        &session_id,
        "completed-run-key",
        &initial_request,
    )
    .await;
    let accepted_status = accepted.status();
    let accepted = json_body(accepted).await;
    assert_eq!(
        accepted_status,
        StatusCode::ACCEPTED,
        "unexpected response: {accepted}"
    );
    assert_eq!(accepted["disposition"], "started");
    let run_id = accepted["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(accepted["run"]["sessionId"], session_id);

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "reasoning.delta",
            "message.delta",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[2].data["data"]["delta"], "Think carefully. ");
    assert_eq!(events[3].data["data"]["delta"], "Hello");
    assert_eq!(events[4].data["data"]["delta"], " world");

    let run = get_json(&harness.app, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(run["status"], "completed");
    assert_eq!(run["lastSequence"], events.len() as u64);
    assert_eq!(
        run["usage"],
        json!({
            "promptTokens": 7,
            "completionTokens": 2,
            "totalTokens": 9,
            "cost": 0.01
        })
    );

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let items = messages["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["role"], "user");
    assert_eq!(
        items[0]["parts"],
        json!([{"type": "text", "text": "hello"}])
    );
    assert_eq!(items[1]["role"], "assistant");
    assert_eq!(
        items[1]["parts"],
        json!([{"type": "text", "text": "Hello world"}])
    );
    assert_eq!(items[1]["reasoning"], "Think carefully. ");
    assert_eq!(items[1]["usage"], run["usage"]);

    let cursor_index = 2;
    let replayed_events = collect_events(
        events_request(
            &harness.app,
            &run_id,
            Some(events[cursor_index].id.as_str()),
        )
        .await,
    )
    .await;
    assert_eq!(replayed_events, events[cursor_index + 1..]);

    let wrong_run = events_request(&harness.app, &run_id, Some("run_wrong:1")).await;
    assert_problem(wrong_run, StatusCode::BAD_REQUEST, "validation_failed").await;
    let future_id = format!("{run_id}:{}", events.len() + 1);
    let future = events_request(&harness.app, &run_id, Some(&future_id)).await;
    assert_problem(future, StatusCode::BAD_REQUEST, "validation_failed").await;

    let replay = post_run(
        &harness.app,
        &session_id,
        "completed-run-key",
        &initial_request,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    let replay = json_body(replay).await;
    assert_eq!(replay["disposition"], "replayed");
    assert_eq!(replay["run"]["id"], run_id);

    let conflict_request = run_request("request-0002", "different body");
    let conflict = post_run(
        &harness.app,
        &session_id,
        "completed-run-key",
        &conflict_request,
    )
    .await;
    assert_problem(conflict, StatusCode::CONFLICT, "idempotency_conflict").await;
    assert_eq!(harness.provider.call_count(), 1);

    let messages_after_replay = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert_eq!(messages_after_replay["items"].as_array().unwrap().len(), 2);

    let session_path = format!("/api/v1/sessions/{session_id}");
    let current_session =
        authorized_request(&harness.app, Method::GET, &session_path, Body::empty()).await;
    assert_eq!(current_session.status(), StatusCode::OK);
    let session_etag = current_session.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let _ = json_body(current_session).await;
    let deleted = authorized_request_builder(
        &harness.app,
        Request::delete(&session_path)
            .header(header::IF_MATCH, session_etag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let retained_run = authorized_request(
        &harness.app,
        Method::GET,
        &format!("/api/v1/runs/{run_id}"),
        Body::empty(),
    )
    .await;
    assert_eq!(retained_run.status(), StatusCode::OK);
    assert_eq!(json_body(retained_run).await["status"], "completed");
    let retained_events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(retained_events, events);

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "test-model");
    assert_eq!(
        requests[0]["messages"],
        json!([{"role": "user", "content": "hello"}])
    );
    assert_eq!(requests[0]["stream"], true);
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn enabled_session_search_executes_a_persisted_bounded_tool_loop() {
    let harness = Harness::new().await;
    harness.enable_session_search().await;
    let session_id = create_session(&harness.app, "tool-loop-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "tool-loop-run",
        &run_request("request-tool-loop", "use session search for needle"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted = json_body(accepted).await;
    let run_id = accepted["run"]["id"].as_str().unwrap().to_owned();

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "session_search");
    assert_eq!(events[3].data["data"]["inputSummary"], "session_search");
    assert_eq!(
        events[4].data["data"]["resultSummary"],
        "1 matching sessions"
    );
    assert_eq!(events[5].data["data"]["delta"], "Found history");

    let run = get_json(&harness.app, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(run["status"], "completed");
    assert_eq!(
        run["usage"],
        json!({
            "promptTokens": 13,
            "completionTokens": 3,
            "totalTokens": 16
        })
    );

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let assistant = &messages["items"][1];
    assert_eq!(assistant["parts"][0]["text"], "Found history");
    assert_eq!(assistant["toolCalls"].as_array().unwrap().len(), 1);
    assert_eq!(assistant["toolCalls"][0]["callId"], "call-search-1");
    assert_eq!(assistant["toolCalls"][0]["status"], "completed");

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["tools"][0]["function"]["name"],
        "session_search"
    );
    assert_eq!(requests[0]["tool_choice"], "auto");
    let continuation = requests[1]["messages"].as_array().unwrap();
    assert_eq!(continuation[1]["role"], "assistant");
    assert_eq!(continuation[1]["tool_calls"][0]["id"], "call-search-1");
    assert_eq!(continuation[2]["role"], "tool");
    assert_eq!(continuation[2]["tool_call_id"], "call-search-1");
    assert!(
        !continuation[2]["content"]
            .as_str()
            .unwrap()
            .contains(harness._home.path().to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn stdio_mcp_tool_is_discovered_approved_executed_and_kept_private() {
    let harness = Harness::new().await;
    let executable = compile_mcp_stdio_fixture(&harness._home);
    let created = authorized_request_builder(
        &harness.app,
        Request::post("/api/v1/profiles/default/mcp/servers")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "mcp-run-fixture-01")
            .body(Body::from(
                json!({
                    "transport": "stdio",
                    "name": "fixture",
                    "command": executable.to_string_lossy(),
                    "args": [],
                    "enabled": true,
                    "timeoutSeconds": 5,
                    "envSecretNames": []
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let session_id = create_session(&harness.app, "mcp-run-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "mcp-run-request",
        &run_request("request-mcp-run", "use mcp echo"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    assert_eq!(waiting["pendingAction"]["toolName"], MCP_TOOL_NAME);
    assert_eq!(
        waiting["pendingAction"]["inputSummary"],
        format!("MCP tool {MCP_TOOL_NAME}")
    );
    let approval_id = waiting["pendingAction"]["approvalId"].as_str().unwrap();
    let approved = post_approval(&harness.app, &run_id, approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert_eq!(completed["status"], "completed");
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert!(!events.iter().any(|event| event.name == "tool.delivery"));
    let mcp_completed = events
        .iter()
        .find(|event| event.name == "tool.completed" && event.data["data"]["callId"] == MCP_CALL_ID)
        .unwrap();
    assert!(
        mcp_completed.data["data"]
            .get("asyncDeliveryPending")
            .is_none()
    );

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"].as_array().unwrap().iter().any(|tool| {
        tool["function"]["name"] == MCP_TOOL_NAME && tool["function"]["strict"] == false
    }));
    let private_content = provider_tool_content(&requests[1], MCP_CALL_ID);
    assert!(private_content.contains(MCP_PRIVATE_RESULT));

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let public = messages.to_string();
    assert!(!public.contains(MCP_PRIVATE_RESULT));
    assert_eq!(messages["items"][1]["toolCalls"][0]["name"], MCP_TOOL_NAME);
    assert_eq!(
        messages["items"][1]["toolCalls"][0]["resultSummary"],
        "MCP tool completed"
    );
}

#[tokio::test]
async fn streamable_http_mcp_tool_is_discovered_approved_executed_and_kept_private() {
    let harness = Harness::new().await;
    let fixture = Arc::new(StreamableRunFixture::default());
    let mcp_app = Router::new()
        .route(
            "/mcp",
            post(streamable_mcp_post).delete(streamable_mcp_delete),
        )
        .with_state(fixture.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mcp_server = tokio::spawn(async move {
        axum::serve(listener, mcp_app).await.unwrap();
    });

    let secret = authorized_request_builder(
        &harness.app,
        Request::put(format!(
            "/api/v1/profiles/default/secrets/{MCP_REMOTE_SECRET_NAME}"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"value": MCP_REMOTE_SECRET}).to_string()))
        .unwrap(),
    )
    .await;
    assert_eq!(secret.status(), StatusCode::OK);
    assert!(
        !json_body(secret)
            .await
            .to_string()
            .contains(MCP_REMOTE_SECRET)
    );

    let created = authorized_request_builder(
        &harness.app,
        Request::post("/api/v1/profiles/default/mcp/servers")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "mcp-streamable-run-01")
            .body(Body::from(
                json!({
                    "transport": "streamableHttp",
                    "name": "fixture",
                    "url": format!("http://{address}/mcp"),
                    "enabled": true,
                    "timeoutSeconds": 5,
                    "bearerTokenSecretName": MCP_REMOTE_SECRET_NAME
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let session_id = create_session(&harness.app, "mcp-streamable-run-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "mcp-streamable-run-request",
        &run_request("request-mcp-streamable-run", "use mcp echo"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    assert_eq!(waiting["pendingAction"]["toolName"], MCP_TOOL_NAME);
    let approval_id = waiting["pendingAction"]["approvalId"].as_str().unwrap();
    let approved = post_approval(&harness.app, &run_id, approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert_eq!(completed["status"], "completed");

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"].as_array().unwrap().iter().any(|tool| {
        tool["function"]["name"] == MCP_TOOL_NAME && tool["function"]["strict"] == false
    }));
    assert!(provider_tool_content(&requests[1], MCP_CALL_ID).contains(MCP_PRIVATE_RESULT));

    let public = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert!(!public.to_string().contains(MCP_PRIVATE_RESULT));
    assert!(!public.to_string().contains(MCP_REMOTE_SECRET));
    assert!(
        fixture
            .methods
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "tools/call")
    );
    mcp_server.abort();
}

#[tokio::test]
async fn legacy_sse_mcp_tool_is_discovered_approved_executed_and_kept_private() {
    let harness = Harness::new().await;
    let fixture = LegacySseRunFixture {
        channels: Arc::new(Mutex::new(HashMap::new())),
        next_session: AtomicUsize::new(1),
    };
    let mcp_app = Router::new()
        .route("/sse", get(legacy_mcp_sse_get))
        .route("/messages", post(legacy_mcp_sse_post))
        .with_state(Arc::new(fixture));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mcp_server = tokio::spawn(async move {
        axum::serve(listener, mcp_app).await.unwrap();
    });

    let secret = authorized_request_builder(
        &harness.app,
        Request::put(format!(
            "/api/v1/profiles/default/secrets/{MCP_REMOTE_SECRET_NAME}"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"value": MCP_REMOTE_SECRET}).to_string()))
        .unwrap(),
    )
    .await;
    assert_eq!(secret.status(), StatusCode::OK);
    let created = authorized_request_builder(
        &harness.app,
        Request::post("/api/v1/profiles/default/mcp/servers")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "mcp-legacy-sse-run-01")
            .body(Body::from(
                json!({
                    "transport": "sse",
                    "name": "fixture",
                    "url": format!("http://{address}/sse"),
                    "enabled": true,
                    "timeoutSeconds": 5,
                    "bearerTokenSecretName": MCP_REMOTE_SECRET_NAME
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let session_id = create_session(&harness.app, "mcp-legacy-sse-run-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "mcp-legacy-sse-run-request",
        &run_request("request-mcp-legacy-sse-run", "use mcp echo"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    assert_eq!(waiting["pendingAction"]["toolName"], MCP_TOOL_NAME);
    let approval_id = waiting["pendingAction"]["approvalId"].as_str().unwrap();
    assert_eq!(
        post_approval(&harness.app, &run_id, approval_id, "once", None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        wait_for_run_status(&harness.app, &run_id, "completed").await["status"],
        "completed"
    );
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(provider_tool_content(&requests[1], MCP_CALL_ID).contains(MCP_PRIVATE_RESULT));
    let public = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert!(!public.to_string().contains(MCP_PRIVATE_RESULT));
    assert!(!public.to_string().contains(MCP_REMOTE_SECRET));
    mcp_server.abort();
}

#[tokio::test]
async fn enabled_skills_list_executes_through_the_persisted_tool_loop() {
    let harness = Harness::new().await;
    harness.install_skill();
    harness.enable_toolset("skills").await;
    let session_id = create_session(&harness.app, "skills-tool-loop-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "skills-tool-loop-run",
        &run_request("request-skills-tool-loop", "use skills list"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted = json_body(accepted).await;
    let run_id = accepted["run"]["id"].as_str().unwrap().to_owned();

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(events[3].name, "tool.started");
    assert_eq!(events[3].data["data"]["name"], "skills_list");
    assert_eq!(events[4].name, "tool.completed");
    assert_eq!(events[4].data["data"]["resultSummary"], "1 enabled skills");
    assert_eq!(events[5].data["data"]["delta"], "Found skills");

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    let definitions = requests[0]["tools"].as_array().unwrap();
    assert_eq!(
        definitions
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["skill_view", "skills_list"]
    );
    let continuation = requests[1]["messages"].as_array().unwrap();
    let tool_result = continuation.last().unwrap();
    assert_eq!(tool_result["role"], "tool");
    assert_eq!(tool_result["tool_call_id"], "call-skills-1");
    assert!(
        tool_result["content"]
            .as_str()
            .unwrap()
            .contains("paper-search")
    );
    assert!(
        !tool_result["content"]
            .as_str()
            .unwrap()
            .contains(harness._home.path().to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn clarification_answer_resumes_provider_privately_and_is_durably_idempotent() {
    let harness = Harness::new().await;
    harness.enable_toolset("clarify").await;
    let session_id = create_session(&harness.app, "clarification-answer-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "clarification-answer-run",
        &run_request("request-clarification-answer", "use clarification freeform"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_clarification(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(waiting["status"], "waitingClarification");
    assert_eq!(pending["kind"], "clarification");
    assert_eq!(pending["question"], CLARIFICATION_QUESTION);
    assert_eq!(pending["choices"], json!([]));
    assert_eq!(pending.as_object().unwrap().len(), 4);
    assert!(pending.get("callId").is_none());
    assert!(pending.get("answer").is_none());
    assert_eq!(harness.provider.call_count(), 1);
    let request_id = pending["requestId"].as_str().unwrap().to_owned();

    let answered = post_clarification(
        &harness.app,
        &run_id,
        &request_id,
        CLARIFICATION_PRIVATE_ANSWER,
    )
    .await;
    assert_eq!(answered.status(), StatusCode::OK);
    assert_eq!(json_body(answered).await, json!({"accepted": true}));

    let replay = post_clarification(
        &harness.app,
        &run_id,
        &request_id,
        CLARIFICATION_PRIVATE_ANSWER,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await, json!({"accepted": true}));

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "clarification.required",
            "clarification.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["callId"], CLARIFICATION_CALL_ID);
    assert_eq!(events[3].data["data"]["name"], "clarify");
    assert_eq!(
        events[3].data["data"]["inputSummary"],
        "Clarification requested"
    );
    assert_eq!(
        events[4].data["data"],
        json!({
            "requestId": request_id,
            "question": CLARIFICATION_QUESTION,
            "choices": [],
        })
    );
    assert_eq!(
        events[5].data["data"],
        json!({
            "requestId": request_id,
            "resolvedBy": "user",
        })
    );
    assert_eq!(
        events[6].data["data"]["resultSummary"],
        "Clarification answered"
    );
    assert_eq!(events[7].data["data"]["delta"], "Clarification handled");

    let completed = get_json(&harness.app, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["pendingAction"], Value::Null);
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let assistant = &messages["items"][1];
    assert_eq!(assistant["parts"][0]["text"], "Clarification handled");
    assert_eq!(assistant["toolCalls"][0]["callId"], CLARIFICATION_CALL_ID);
    assert_eq!(assistant["toolCalls"][0]["name"], "clarify");
    assert_eq!(assistant["toolCalls"][0]["status"], "completed");
    assert_eq!(
        assistant["toolCalls"][0]["inputSummary"],
        "Clarification requested"
    );
    assert_eq!(
        assistant["toolCalls"][0]["resultSummary"],
        "Clarification answered"
    );
    let public_state = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        completed,
        messages,
    );
    assert!(!public_state.contains("PRIVATE_CLARIFICATION_ANSWER_DO_NOT_EXPOSE"));

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["tools"][0]["function"]["name"], "clarify");
    let continuation = requests[1]["messages"].as_array().unwrap();
    assert_eq!(continuation[1]["role"], "assistant");
    assert_eq!(
        continuation[1]["tool_calls"][0]["id"],
        CLARIFICATION_CALL_ID
    );
    assert_eq!(continuation[2]["role"], "tool");
    let private_result: Value =
        serde_json::from_str(provider_tool_content(&requests[1], CLARIFICATION_CALL_ID)).unwrap();
    assert_eq!(
        private_result,
        json!({"answer": CLARIFICATION_PRIVATE_ANSWER})
    );

    let conflict = post_clarification(
        &harness.app,
        &run_id,
        &request_id,
        "DIFFERENT_PRIVATE_ANSWER",
    )
    .await;
    assert_problem(
        conflict,
        StatusCode::CONFLICT,
        "clarification_answer_conflict",
    )
    .await;
    assert_eq!(harness.provider.call_count(), 2);
}

#[tokio::test]
async fn clarification_rejects_an_unoffered_choice_without_advancing_the_run() {
    let harness = Harness::new().await;
    harness.enable_toolset("clarify").await;
    let session_id = create_session(&harness.app, "clarification-choice-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "clarification-choice-run",
        &run_request("request-clarification-choice", "use clarification choice"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_clarification(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(pending["choices"], json!(["staging", "production"]));
    let request_id = pending["requestId"].as_str().unwrap().to_owned();
    let sequence = waiting["lastSequence"].as_u64().unwrap();

    let invalid = post_clarification(&harness.app, &run_id, &request_id, "preview").await;
    assert_problem(
        invalid,
        StatusCode::CONFLICT,
        "clarification_choice_not_offered",
    )
    .await;
    let unchanged = get_json(&harness.app, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(unchanged["status"], "waitingClarification");
    assert_eq!(unchanged["lastSequence"], sequence);
    assert_eq!(unchanged["pendingAction"], *pending);
    assert_eq!(harness.provider.call_count(), 1);

    let valid = post_clarification(&harness.app, &run_id, &request_id, "production").await;
    assert_eq!(valid.status(), StatusCode::OK);
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(events.last().unwrap().name, "run.completed");
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(provider_tool_content(&requests[1], CLARIFICATION_CALL_ID,))
            .unwrap(),
        json!({"answer": "production"})
    );
}

#[tokio::test]
async fn clarification_cancel_first_rejects_a_late_answer_without_provider_continuation() {
    let harness = Harness::new().await;
    harness.enable_toolset("clarify").await;
    let session_id = create_session(&harness.app, "clarification-cancel-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "clarification-cancel-run",
        &run_request(
            "request-clarification-cancel",
            "use clarification freeform cancel",
        ),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_clarification(&harness.app, &run_id).await;
    let request_id = waiting["pendingAction"]["requestId"]
        .as_str()
        .unwrap()
        .to_owned();

    let cancel = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{run_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);
    let _ = json_body(cancel).await;
    let cancelled = wait_for_run_status(&harness.app, &run_id, "cancelled").await;
    assert_eq!(cancelled["pendingAction"], Value::Null);

    let late = post_clarification(
        &harness.app,
        &run_id,
        &request_id,
        CLARIFICATION_PRIVATE_ANSWER,
    )
    .await;
    assert_problem(
        late,
        StatusCode::CONFLICT,
        "clarification_no_longer_pending",
    )
    .await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "clarification.required",
            "clarification.resolved",
            "tool.failed",
            "run.cancelled",
        ]
    );
    assert_eq!(
        events[5].data["data"],
        json!({
            "requestId": request_id,
            "resolvedBy": "cancellation",
        })
    );
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "tool_execution_cancelled"
    );
    assert!(
        !events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>()
            .contains("PRIVATE_CLARIFICATION_ANSWER_DO_NOT_EXPOSE")
    );
    assert_eq!(harness.provider.call_count(), 1);
}

#[tokio::test]
async fn workspace_read_file_is_run_scoped_persisted_and_does_not_leak_secrets() {
    let harness = Harness::new().await;
    harness.enable_toolset("file").await;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    std::fs::write(
        workspace.path().join("src/lib.rs"),
        "pub fn answer() -> usize { 42 }\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join(".env"), "TOKEN=private\n").unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;

    let unscoped_session = create_session(&harness.app, "file-unscoped-session").await;
    let unscoped = post_run(
        &harness.app,
        &unscoped_session,
        "file-unscoped-run",
        &run_request("request-file-unscoped", "hello without workspace"),
    )
    .await;
    assert_eq!(unscoped.status(), StatusCode::ACCEPTED);
    let unscoped_run_id = json_body(unscoped).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let _ = collect_events(events_request(&harness.app, &unscoped_run_id, None).await).await;

    let session_id = create_session(&harness.app, "file-workspace-session").await;
    let mut request = run_request("request-file-workspace", "use workspace file");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "file-workspace-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(events[3].name, "tool.started");
    assert_eq!(events[3].data["data"]["name"], "read_file");
    assert_eq!(events[4].name, "tool.completed");
    assert_eq!(
        events[4].data["data"]["resultSummary"],
        "1 lines from src/lib.rs"
    );
    assert_eq!(events[5].data["data"]["delta"], "Read workspace");

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let assistant = &messages["items"][1];
    assert_eq!(assistant["toolCalls"][0]["callId"], "call-read-file-1");
    assert_eq!(assistant["toolCalls"][0]["status"], "completed");

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].get("tools").is_none());
    assert_eq!(
        requests[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["patch", "read_file", "search_files", "write_file"]
    );
    let continuation = requests[2]["messages"].as_array().unwrap();
    assert_eq!(continuation.last().unwrap()["role"], "tool");
    assert_eq!(
        continuation.last().unwrap()["tool_call_id"],
        "call-read-file-1"
    );
    assert!(
        continuation.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("answer")
    );

    let private_root = workspace.path().to_string_lossy();
    let persisted = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        messages,
        Value::Array(requests)
    );
    assert!(!persisted.contains(private_root.as_ref()));
    assert!(!persisted.contains("TOKEN=private"));
}

#[tokio::test]
async fn workspace_write_file_once_waits_for_approval_then_completes_without_public_leaks() {
    let harness = Harness::new().await;
    harness.enable_toolset("file").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "write-once-session").await;
    let mut request = run_request("request-write-once", "use workspace write once");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "write-once-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(waiting["status"], "waitingApproval");
    assert_eq!(pending["kind"], "approval");
    assert_eq!(pending["callId"], "call-write-file-1");
    assert_eq!(pending["toolName"], "write_file");
    assert_eq!(pending["choices"], json!(["once", "deny"]));
    assert_eq!(
        pending["inputSummary"],
        format!("Write {WRITE_PATH} ({} bytes)", WRITE_CONTENT.len())
    );
    assert!(!workspace.path().join(WRITE_PATH).exists());
    assert_public_write_state_redacted(&[], &waiting, workspace.path());

    let approval_id = pending["approvalId"].as_str().unwrap();
    let approved = post_approval(&harness.app, &run_id, approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);
    assert_eq!(json_body(approved).await, json!({"accepted": true}));

    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(WRITE_PATH)).unwrap(),
        WRITE_CONTENT
    );
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "write_file");
    assert_eq!(events[4].data["data"]["approvalId"], approval_id);
    assert_eq!(events[5].data["data"]["decision"], "once");
    assert_eq!(events[5].data["data"]["resolvedBy"], "user");
    assert_eq!(
        events[6].data["data"]["resultSummary"],
        format!("Wrote {} bytes to {WRITE_PATH}", WRITE_CONTENT.len())
    );
    assert_eq!(events[7].data["data"]["delta"], "Write handled");
    assert_public_write_state_redacted(&events, &completed, workspace.path());

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]["messages"].as_array().unwrap();
    assert_eq!(continuation.last().unwrap()["role"], "tool");
    assert_eq!(
        continuation.last().unwrap()["tool_call_id"],
        "call-write-file-1"
    );
}

#[tokio::test]
async fn browser_download_run_requires_a_durable_owner_bound_approval() {
    let harness = Harness::new().await;
    let capabilities = get_json(&harness.app, "/api/v1/capabilities").await;
    if capabilities["extensions"]["browserDownloads"] != true {
        // The BrowserManager only advertises this capability when a supported
        // Chromium binary is installed. The deterministic CDP fixture still
        // covers the protocol path on hosts without one.
        return;
    }
    harness.enable_toolset("browser").await;
    let session_id = create_session(&harness.app, "browser-download-run-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "browser-download-run-request",
        &run_request("request-browser-download", "use browser download approval"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(pending["callId"], BROWSER_DOWNLOAD_CALL_ID);
    assert_eq!(pending["toolName"], "browser_download");
    assert_eq!(pending["choices"], json!(["once", "deny"]));
    assert!(pending["inputSummary"].as_str().is_some_and(|summary| {
        summary.starts_with("Download browser resource from a#report [args sha256:")
    }));

    let approval_id = pending["approvalId"].as_str().unwrap();
    let denied = post_approval(&harness.app, &run_id, approval_id, "deny", None).await;
    assert_eq!(denied.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert_eq!(completed["status"], "completed");

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert!(events.iter().any(|event| {
        event.name == "approval.required" && event.data["data"]["toolName"] == "browser_download"
    }));
    assert!(events.iter().any(|event| {
        event.name == "approval.resolved" && event.data["data"]["decision"] == "deny"
    }));
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"].as_array().unwrap().iter().any(|tool| {
        tool["function"]["name"] == "browser_download" && tool["function"]["strict"] == true
    }));
    let public = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        completed,
        get_json(
            &harness.app,
            &format!("/api/v1/sessions/{session_id}/messages"),
        )
        .await,
    );
    assert!(!public.contains(harness._home.path().to_string_lossy().as_ref()));
}

#[tokio::test]
async fn memory_write_is_approved_privately_and_only_changes_the_next_run_snapshot() {
    let harness = Harness::new().await;
    harness.enable_toolset("memory").await;
    let memory_dir = harness._home.path().join("memories");
    std::fs::create_dir_all(&memory_dir).unwrap();
    let memory_path = memory_dir.join("MEMORY.md");
    std::fs::write(&memory_path, INITIAL_MEMORY_CONTENT).unwrap();
    let session_id = create_session(&harness.app, "memory-write-session").await;

    let accepted = post_run(
        &harness.app,
        &session_id,
        "memory-write-run",
        &run_request("request-memory-write", "use memory write once"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(pending["callId"], MEMORY_CALL_ID);
    assert_eq!(pending["toolName"], "memory");
    assert_eq!(pending["choices"], json!(["once", "deny"]));
    let approval_summary = pending["inputSummary"].as_str().unwrap();
    assert!(approval_summary.starts_with("Update persistent memory [args sha256:"));
    assert!(!approval_summary.contains(MEMORY_WRITE_CONTENT));
    assert_eq!(
        std::fs::read_to_string(&memory_path).unwrap(),
        INITIAL_MEMORY_CONTENT
    );

    let approval_id = pending["approvalId"].as_str().unwrap();
    let approved = post_approval(&harness.app, &run_id, approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let persisted = std::fs::read_to_string(&memory_path).unwrap();
    assert!(persisted.contains(INITIAL_MEMORY_CONTENT));
    assert!(persisted.contains(MEMORY_WRITE_CONTENT));

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "memory");
    assert_eq!(
        events[6].data["data"]["resultSummary"],
        "Persistent memory updated"
    );
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let public = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        completed,
        messages,
    );
    assert!(!public.contains(MEMORY_WRITE_CONTENT));
    assert!(!public.contains(INITIAL_MEMORY_CONTENT));

    let first_requests = harness.provider.requests();
    assert_eq!(first_requests.len(), 2);
    for request in &first_requests {
        let prompt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "system")
            .and_then(|message| message["content"].as_str())
            .unwrap();
        assert!(prompt.contains(INITIAL_MEMORY_CONTENT));
        assert!(!prompt.contains(MEMORY_WRITE_CONTENT));
    }
    assert!(
        first_requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "memory")
    );

    let next = post_run(
        &harness.app,
        &session_id,
        "memory-next-run",
        &run_request("request-memory-next", "verify the next memory snapshot"),
    )
    .await;
    assert_eq!(next.status(), StatusCode::ACCEPTED);
    let next_run_id = json_body(next).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let _ = wait_for_run_status(&harness.app, &next_run_id, "completed").await;
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 3);
    let next_prompt = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "system")
        .and_then(|message| message["content"].as_str())
        .unwrap();
    assert!(next_prompt.contains(INITIAL_MEMORY_CONTENT));
    assert!(next_prompt.contains(MEMORY_WRITE_CONTENT));
}

#[tokio::test]
async fn execute_code_waits_for_approval_runs_under_guardian_and_keeps_rpc_private() {
    let harness = Harness::new().await;
    let capabilities = get_json(&harness.app, "/api/v1/capabilities").await;
    if capabilities["extensions"]["codeExecution"] != true {
        return;
    }
    harness.enable_toolset("file").await;
    harness.enable_toolset("code_execution").await;
    let secret = authorized_request_builder(
        &harness.app,
        Request::put("/api/v1/profiles/default/secrets/OPENAI_API_KEY")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"value": CODE_PROFILE_SECRET}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(secret.status(), StatusCode::OK);
    assert!(
        !json_body(secret)
            .await
            .to_string()
            .contains(CODE_PROFILE_SECRET)
    );
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("input.txt"), CODE_INPUT_CONTENT).unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "execute-code-session").await;
    let mut request = run_request("request-execute-code", "use execute code once");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "execute-code-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(pending["callId"], CODE_CALL_ID);
    assert_eq!(pending["toolName"], "execute_code");
    assert_eq!(pending["choices"], json!(["once", "deny"]));
    let approval_summary = pending["inputSummary"].as_str().unwrap();
    assert!(approval_summary.starts_with("Execute host-authority Python script"));
    assert!(approval_summary.contains("[args sha256:"));
    assert!(!approval_summary.contains(workspace.path().to_string_lossy().as_ref()));
    assert!(!workspace.path().join(CODE_FILE_PATH).exists());

    let approval_id = pending["approvalId"].as_str().unwrap();
    let approved = post_approval(&harness.app, &run_id, approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let requests = harness.provider.requests();
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(CODE_FILE_PATH)).unwrap_or_else(|error| {
            panic!(
                "execute_code did not create its approved file: {error}; run={completed}; requests={requests:?}"
            )
        }),
        CODE_FILE_CONTENT
    );

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "execute_code");
    assert_eq!(
        events[4].data["data"]["inputSummary"],
        "Execute Python programmatic tool script"
    );
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    let public = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        completed,
        messages,
    );
    for private in [
        CODE_OUTPUT,
        CODE_INPUT_CONTENT,
        CODE_FILE_CONTENT,
        CODE_PROFILE_SECRET,
        CODE_FILE_PATH,
        "from hermes_tools import read_file",
    ] {
        assert!(!public.contains(private), "public state leaked {private}");
    }

    assert_eq!(requests.len(), 2);
    let execute_definition = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["function"]["name"] == "execute_code")
        .expect("execute_code should be injected only when the interpreter is ready");
    assert!(
        execute_definition["function"]["description"]
            .as_str()
            .unwrap()
            .contains("read_file(path")
    );
    let provider_content = provider_tool_content(&requests[1], CODE_CALL_ID);
    assert!(provider_content.contains(CODE_OUTPUT));
    assert!(provider_content.contains(CODE_ENV_ABSENT));
    assert!(!provider_content.contains(CODE_PROFILE_SECRET));
    assert!(provider_content.contains("\"status\":\"success\""));

    let connection =
        rusqlite::Connection::open(harness._home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let nested: (String, String, i64, String, String) = connection
        .query_row(
            "SELECT parent_call_id, tool_name, rpc_sequence, status, arguments_json \
             FROM tool_invocations WHERE run_id = ?1 AND origin = 'codeRpc'",
            [&run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(nested.0, CODE_CALL_ID);
    assert_eq!(nested.1, "read_file");
    assert_eq!(nested.2, 1);
    assert_eq!(nested.3, "completed");
    assert!(nested.4.contains("input.txt"));
}

#[tokio::test]
async fn execute_code_deny_never_spawns_or_plans_nested_rpc_calls() {
    let harness = Harness::new().await;
    let capabilities = get_json(&harness.app, "/api/v1/capabilities").await;
    if capabilities["extensions"]["codeExecution"] != true {
        return;
    }
    harness.enable_toolset("file").await;
    harness.enable_toolset("code_execution").await;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("input.txt"), CODE_INPUT_CONTENT).unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "execute-code-deny-session").await;
    let mut request = run_request("request-execute-code-deny", "use execute code deny");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "execute-code-deny-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let approval_id = waiting["pendingAction"]["approvalId"]
        .as_str()
        .unwrap()
        .to_owned();
    let denied = post_approval(
        &harness.app,
        &run_id,
        &approval_id,
        "deny",
        Some("do not execute host code"),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert!(!workspace.path().join(CODE_FILE_PATH).exists());
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.failed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[5].data["data"]["decision"], "deny");
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "tool_execution_denied"
    );
    let public = format!(
        "{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        completed,
    );
    for private in [
        CODE_OUTPUT,
        CODE_INPUT_CONTENT,
        CODE_FILE_CONTENT,
        CODE_FILE_PATH,
    ] {
        assert!(!public.contains(private));
    }
    let connection =
        rusqlite::Connection::open(harness._home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let nested_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM tool_invocations WHERE run_id = ?1 AND origin = 'codeRpc'",
            [&run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(nested_count, 0);
}

#[tokio::test]
async fn cancelling_execute_code_kills_the_guarded_python_tree() {
    let harness = Harness::new().await;
    let capabilities = get_json(&harness.app, "/api/v1/capabilities").await;
    if capabilities["extensions"]["codeExecution"] != true {
        return;
    }
    harness.enable_toolset("code_execution").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "execute-code-cancel-session").await;
    let mut request = run_request(
        "request-execute-code-slow",
        "use execute code slow and wait",
    );
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(
        &harness.app,
        &session_id,
        "execute-code-cancel-run",
        &request,
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let approval_id = waiting["pendingAction"]["approvalId"]
        .as_str()
        .unwrap()
        .to_owned();
    let approved = post_approval(&harness.app, &run_id, &approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);

    let heartbeat = workspace.path().join(CODE_HEARTBEAT_PATH);
    timeout(WAIT_TIMEOUT, async {
        loop {
            if std::fs::metadata(&heartbeat).is_ok_and(|metadata| metadata.len() >= 2) {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("approved Python should start writing its heartbeat");
    let cancel = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{run_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);
    let cancelled = wait_for_run_status(&harness.app, &run_id, "cancelled").await;
    let stopped_at = std::fs::metadata(&heartbeat).unwrap().len();
    sleep(Duration::from_millis(500)).await;
    assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), stopped_at);
    assert_eq!(harness.provider.call_count(), 1);

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    let names = event_names(&events);
    let connection =
        rusqlite::Connection::open(harness._home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let invocation_status: String = connection
        .query_row(
            "SELECT status FROM tool_invocations WHERE run_id = ?1 AND call_id = ?2",
            [&run_id, CODE_CALL_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(names.last(), Some(&"run.cancelled"));
    assert!(
        names.contains(&"tool.failed"),
        "cancelled invocation remained {invocation_status}; events={names:?}"
    );
    assert!(!names.contains(&"tool.completed"));
    let public = format!(
        "{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        cancelled,
    );
    assert!(!public.contains(CODE_HEARTBEAT_PATH));
    assert!(!public.contains("while True"));
}

#[tokio::test]
async fn terminal_foreground_spawns_only_after_once_approval_and_keeps_public_state_redacted() {
    let harness = Harness::new().await;
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "terminal-foreground-session").await;
    let mut request = run_request(
        "request-terminal-foreground",
        "use terminal foreground once",
    );
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(
        &harness.app,
        &session_id,
        "terminal-foreground-run",
        &request,
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(pending["toolName"], "terminal");
    assert_eq!(pending["callId"], TERMINAL_CALL_ID);
    assert_eq!(pending["choices"], json!(["once", "deny"]));
    let approval_summary = pending["inputSummary"].as_str().unwrap();
    assert!(approval_summary.starts_with("Run terminal command (foreground): `"));
    assert!(approval_summary.contains(TERMINAL_OUTPUT));
    assert!(approval_summary.contains(TERMINAL_PATH));
    assert!(approval_summary.contains("[args sha256:"));
    assert!(!workspace.path().join(TERMINAL_PATH).exists());
    assert!(!approval_summary.contains(workspace.path().to_string_lossy().as_ref()));

    let approval_id = pending["approvalId"].as_str().unwrap();
    let approved = post_approval(&harness.app, &run_id, approval_id, "once", None).await;
    assert_eq!(approved.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(TERMINAL_PATH)).unwrap(),
        TERMINAL_FILE_CONTENT.trim_end()
    );

    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "terminal");
    assert_eq!(
        events[6].data["data"]["resultSummary"],
        "Terminal command exited with code 0"
    );
    assert_public_terminal_state_redacted(&events, &completed, workspace.path());

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert_public_terminal_state_redacted(&events, &messages, workspace.path());
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["process", "terminal"]
    );
    let provider_content = provider_tool_content(&requests[1], TERMINAL_CALL_ID);
    assert!(provider_content.contains(TERMINAL_OUTPUT));
    assert!(provider_content.contains("\"exit_code\":0"));
}

#[tokio::test]
async fn background_terminal_poll_and_kill_use_dynamic_risk_and_two_durable_approvals() {
    let harness = Harness::new().await;
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "terminal-process-session").await;
    let mut request = run_request("request-terminal-process", "use terminal process lifecycle");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "terminal-process-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let first_waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let first_pending = &first_waiting["pendingAction"];
    assert_eq!(first_pending["toolName"], "terminal");
    assert_eq!(first_pending["callId"], BACKGROUND_TERMINAL_CALL_ID);
    let background_approval_summary = first_pending["inputSummary"].as_str().unwrap();
    assert!(background_approval_summary.starts_with("Run terminal command (background): `"));
    assert!(background_approval_summary.contains("while true"));
    assert!(background_approval_summary.contains(BACKGROUND_OUTPUT));
    assert!(background_approval_summary.contains("[args sha256:"));
    let db_path = harness._home.path().join(".synthchat/sessions-v1.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let preapproval_processes: i64 = connection
        .query_row("SELECT COUNT(*) FROM terminal_processes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(preapproval_processes, 0);
    drop(connection);

    let first_approval_id = first_pending["approvalId"].as_str().unwrap();
    let first_approved =
        post_approval(&harness.app, &run_id, first_approval_id, "once", None).await;
    assert_eq!(first_approved.status(), StatusCode::OK);

    let second_waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let second_pending = &second_waiting["pendingAction"];
    assert_eq!(second_pending["toolName"], "process");
    assert_eq!(second_pending["callId"], BACKGROUND_KILL_CALL_ID);
    let kill_approval_summary = second_pending["inputSummary"].as_str().unwrap();
    assert!(kill_approval_summary.starts_with("Kill process process_"));
    assert!(kill_approval_summary.contains("[args sha256:"));
    assert_eq!(harness.provider.call_count(), 3);

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let running: (String, String, Option<i64>, usize) = connection
        .query_row(
            "SELECT status, command_preview, pid, length(command_sha256) \
             FROM terminal_processes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(running.0, "running");
    assert!(running.1.starts_with("command sha256:"));
    assert!(!running.1.contains(BACKGROUND_OUTPUT));
    assert!(running.2.is_some());
    assert_eq!(running.3, 64);
    drop(connection);

    let second_approval_id = second_pending["approvalId"].as_str().unwrap();
    let second_approved =
        post_approval(&harness.app, &run_id, second_approval_id, "once", None).await;
    assert_eq!(second_approved.status(), StatusCode::OK);
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "usage.updated",
            "tool.started",
            "tool.completed",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "terminal");
    assert_eq!(events[8].data["data"]["name"], "process");
    assert_eq!(events[8].data["data"]["inputSummary"], "Process poll");
    assert_eq!(events[11].data["data"]["name"], "process");
    assert_eq!(events[11].data["data"]["inputSummary"], "Process kill");
    assert_eq!(events[12].data["data"]["approvalId"], second_approval_id);
    assert_eq!(
        events[14].data["data"]["resultSummary"],
        "Process kill returned killed"
    );

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert_eq!(
        messages["items"][1]["toolCalls"].as_array().unwrap().len(),
        3
    );
    assert_public_process_state_redacted(&events, &completed, &messages, workspace.path());

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 4);
    let background_result: Value = serde_json::from_str(provider_tool_content(
        &requests[1],
        BACKGROUND_TERMINAL_CALL_ID,
    ))
    .unwrap();
    let process_id = background_result["session_id"].as_str().unwrap();
    assert!(process_id.starts_with("process_"));
    let poll_result: Value =
        serde_json::from_str(provider_tool_content(&requests[2], BACKGROUND_POLL_CALL_ID)).unwrap();
    assert_eq!(poll_result["session_id"], process_id);
    assert_eq!(poll_result["status"], "running");
    let kill_result: Value =
        serde_json::from_str(provider_tool_content(&requests[3], BACKGROUND_KILL_CALL_ID)).unwrap();
    assert_eq!(kill_result["session_id"], process_id);
    assert_eq!(kill_result["status"], "killed");

    let connection = rusqlite::Connection::open(db_path).unwrap();
    let terminal_status: String = connection
        .query_row("SELECT status FROM terminal_processes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(terminal_status, "killed");
}

#[tokio::test]
async fn async_completion_delivery_is_durable_once_and_publicly_redacted() {
    let harness = Harness::new().await;
    let capabilities = get_json(&harness.app, "/api/v1/capabilities").await;
    assert_eq!(
        capabilities["engine"]["features"]["asyncToolDelivery"],
        true
    );
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "async-completion-session").await;
    let mut request = run_request("request-async-completion", "use async completion delivery");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "async-completion-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    assert_eq!(waiting["pendingAction"]["callId"], ASYNC_COMPLETION_CALL_ID);
    let approval_id = waiting["pendingAction"]["approvalId"].as_str().unwrap();
    assert_eq!(
        post_approval(&harness.app, &run_id, approval_id, "once", None)
            .await
            .status(),
        StatusCode::OK
    );
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let _ = wait_for_run_sequence(&harness.app, &run_id, 12).await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    let deliveries = events
        .iter()
        .filter(|event| event.name == "tool.delivery")
        .collect::<Vec<_>>();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].data["data"]["callId"],
        ASYNC_COMPLETION_CALL_ID
    );
    assert_eq!(deliveries[0].data["data"]["delivery"], "completion");
    assert_eq!(deliveries[0].data["data"]["status"], "exited");
    assert!(deliveries[0].data["data"].get("exitCode").is_some());
    assert_public_async_delivery_redacted(&events, &completed, workspace.path());

    let connection =
        rusqlite::Connection::open(harness._home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let delivery: (String, Option<i64>) = connection
        .query_row(
            "SELECT state, matched_pattern_count FROM async_tool_deliveries",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(delivery.0, "delivered");
    assert_eq!(delivery.1, None);
}

#[tokio::test]
async fn async_watch_delivery_triggers_once_without_exposing_pattern_or_output() {
    let harness = Harness::new().await;
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "async-watch-session").await;
    let mut request = run_request("request-async-watch", "use async watch delivery");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "async-watch-run", &request).await;
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let approval_id = waiting["pendingAction"]["approvalId"].as_str().unwrap();
    assert_eq!(
        post_approval(&harness.app, &run_id, approval_id, "once", None)
            .await
            .status(),
        StatusCode::OK
    );
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let _ = wait_for_run_sequence(&harness.app, &run_id, 12).await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    let deliveries = events
        .iter()
        .filter(|event| event.name == "tool.delivery")
        .collect::<Vec<_>>();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].data["data"]["callId"], ASYNC_WATCH_CALL_ID);
    assert_eq!(deliveries[0].data["data"]["delivery"], "watch");
    assert_eq!(deliveries[0].data["data"]["matchedPatternCount"], 1);
    assert_public_async_delivery_redacted(&events, &completed, workspace.path());
}

#[tokio::test]
async fn async_completion_delivery_observes_explicit_process_cancellation_once() {
    let harness = Harness::new().await;
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "async-cancel-session").await;
    let mut request = run_request("request-async-cancel", "use async delivery cancellation");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "async-cancel-run", &request).await;
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let terminal_pending = wait_for_pending_approval(&harness.app, &run_id).await;
    assert_eq!(
        terminal_pending["pendingAction"]["callId"],
        ASYNC_CANCEL_CALL_ID
    );
    let terminal_approval = terminal_pending["pendingAction"]["approvalId"]
        .as_str()
        .unwrap();
    assert_eq!(
        post_approval(&harness.app, &run_id, terminal_approval, "once", None)
            .await
            .status(),
        StatusCode::OK
    );
    let kill_pending = wait_for_pending_approval(&harness.app, &run_id).await;
    assert_eq!(
        kill_pending["pendingAction"]["callId"],
        ASYNC_CANCEL_KILL_CALL_ID
    );
    let kill_approval = kill_pending["pendingAction"]["approvalId"]
        .as_str()
        .unwrap();
    assert_eq!(
        post_approval(&harness.app, &run_id, kill_approval, "once", None)
            .await
            .status(),
        StatusCode::OK
    );
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let _ = wait_for_run_sequence(&harness.app, &run_id, 17).await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    let deliveries = events
        .iter()
        .filter(|event| event.name == "tool.delivery")
        .collect::<Vec<_>>();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].data["data"]["delivery"], "completion");
    assert_eq!(deliveries[0].data["data"]["status"], "killed");
    assert_public_async_delivery_redacted(&events, &completed, workspace.path());
}

#[tokio::test]
async fn async_delivery_recovery_and_concurrent_schedulers_publish_one_event() {
    let harness = Harness::new().await;
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "async-restart-session").await;
    let mut request = run_request(
        "request-async-restart",
        "use async completion delivery with async delivery restart recovery",
    );
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "async-restart-run", &request).await;
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let approval_id = waiting["pendingAction"]["approvalId"].as_str().unwrap();
    assert_eq!(
        post_approval(&harness.app, &run_id, approval_id, "once", None)
            .await
            .status(),
        StatusCode::OK
    );
    let _ = wait_for_run_status(&harness.app, &run_id, "completed").await;

    let restarted = harness.restarted_app();
    let _ = wait_for_run_sequence(&restarted, &run_id, 12).await;
    let events = collect_events(events_request(&restarted, &run_id, None).await).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name == "tool.delivery")
            .count(),
        1
    );
    let connection =
        rusqlite::Connection::open(harness._home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let state: String = connection
        .query_row("SELECT state FROM async_tool_deliveries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(state, "delivered");
}

#[tokio::test]
async fn invalid_async_terminal_request_fails_without_process_or_delivery_record() {
    let harness = Harness::new().await;
    harness.enable_toolset("terminal").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "async-invalid-session").await;
    let mut request = run_request("request-async-invalid", "use invalid async delivery");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "async-invalid-run", &request).await;
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert!(event_names(&events).contains(&"tool.failed"));
    assert!(!event_names(&events).contains(&"tool.delivery"));
    assert_public_async_delivery_redacted(&events, &completed, workspace.path());
    let connection =
        rusqlite::Connection::open(harness._home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let processes: i64 = connection
        .query_row("SELECT COUNT(*) FROM terminal_processes", [], |row| {
            row.get(0)
        })
        .unwrap();
    let deliveries: i64 = connection
        .query_row("SELECT COUNT(*) FROM async_tool_deliveries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(processes, 0);
    assert_eq!(deliveries, 0);
}

#[tokio::test]
async fn workspace_write_file_deny_is_idempotent_and_provider_continues_without_writing() {
    let harness = Harness::new().await;
    harness.enable_toolset("file").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "write-deny-session").await;
    let mut request = run_request("request-write-deny", "use workspace write deny");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "write-deny-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let approval_id = waiting["pendingAction"]["approvalId"]
        .as_str()
        .unwrap()
        .to_owned();

    let denied = post_approval(
        &harness.app,
        &run_id,
        &approval_id,
        "deny",
        Some("not for this run"),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(json_body(denied).await, json!({"accepted": true}));
    let completed = wait_for_run_status(&harness.app, &run_id, "completed").await;
    assert!(!workspace.path().join(WRITE_PATH).exists());
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.failed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[5].data["data"]["decision"], "deny");
    assert_eq!(events[5].data["data"]["resolvedBy"], "user");
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "tool_execution_denied"
    );
    assert_eq!(events[7].data["data"]["delta"], "Write handled");
    assert_public_write_state_redacted(&events, &completed, workspace.path());
    assert_eq!(harness.provider.call_count(), 2);

    let sequence = completed["lastSequence"].as_u64().unwrap();
    let replay = post_approval(
        &harness.app,
        &run_id,
        &approval_id,
        "deny",
        Some("not for this run"),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await, json!({"accepted": true}));
    assert_eq!(
        get_json(&harness.app, &format!("/api/v1/runs/{run_id}")).await["lastSequence"],
        sequence
    );
    let conflict = post_approval(
        &harness.app,
        &run_id,
        &approval_id,
        "deny",
        Some("different reason"),
    )
    .await;
    assert_problem(conflict, StatusCode::CONFLICT, "approval_decision_conflict").await;
    assert_eq!(harness.provider.call_count(), 2);
    assert!(!workspace.path().join(WRITE_PATH).exists());
}

#[tokio::test]
async fn workspace_write_file_cancel_first_fails_closed_and_rejects_late_approval() {
    let harness = Harness::new().await;
    harness.enable_toolset("file").await;
    let workspace = tempfile::tempdir().unwrap();
    let workspace_id = register_workspace(&harness.app, workspace.path()).await;
    let session_id = create_session(&harness.app, "write-cancel-session").await;
    let mut request = run_request("request-write-cancel", "use workspace write cancel");
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(&harness.app, &session_id, "write-cancel-run", &request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let approval_id = waiting["pendingAction"]["approvalId"]
        .as_str()
        .unwrap()
        .to_owned();

    let cancel = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{run_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);
    let _ = json_body(cancel).await;
    let cancelled = wait_for_run_status(&harness.app, &run_id, "cancelled").await;
    assert!(!workspace.path().join(WRITE_PATH).exists());

    let late = post_approval(&harness.app, &run_id, &approval_id, "once", None).await;
    assert_problem(late, StatusCode::CONFLICT, "approval_no_longer_pending").await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.failed",
            "run.cancelled",
        ]
    );
    assert_eq!(events[5].data["data"]["decision"], "deny");
    assert_eq!(events[5].data["data"]["resolvedBy"], "cancellation");
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "tool_execution_cancelled"
    );
    assert_public_write_state_redacted(&events, &cancelled, workspace.path());
    assert_eq!(harness.provider.call_count(), 1);
}

#[tokio::test]
async fn workspace_patch_replace_once_applies_after_approval_with_private_real_diff() {
    let harness = Harness::new().await;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    let original = format!("before\n{PATCH_REPLACE_OLD}\nafter\n");
    std::fs::write(workspace.path().join(PATCH_REPLACE_PATH), &original).unwrap();
    let input_summary = format!("Patch {PATCH_REPLACE_PATH} (+1/-1 lines, one)");
    let pending = start_patch_run_waiting_for_approval(
        &harness,
        workspace.path(),
        "replace-once",
        "use workspace patch replace once",
        PATCH_REPLACE_CALL_ID,
        &input_summary,
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        original
    );
    let waiting_messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_public_patch_state_redacted(
        &[],
        &pending.waiting,
        &waiting_messages,
        workspace.path(),
        &[PATCH_REPLACE_OLD, PATCH_REPLACE_NEW],
    );

    let approved = post_approval(
        &harness.app,
        &pending.run_id,
        &pending.approval_id,
        "once",
        None,
    )
    .await;
    assert_eq!(approved.status(), StatusCode::OK);
    assert_eq!(json_body(approved).await, json!({"accepted": true}));
    let completed = wait_for_run_status(&harness.app, &pending.run_id, "completed").await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        format!("before\n{PATCH_REPLACE_NEW}\nafter\n")
    );

    let events = collect_events(events_request(&harness.app, &pending.run_id, None).await).await;
    assert_patch_once_journal(&events, &pending, PATCH_REPLACE_CALL_ID, &input_summary);
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_completed_patch_message(&messages, PATCH_REPLACE_CALL_ID);
    assert_public_patch_state_redacted(
        &events,
        &completed,
        &messages,
        workspace.path(),
        &[PATCH_REPLACE_OLD, PATCH_REPLACE_NEW],
    );

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    let provider_content = provider_tool_content(&requests[1], PATCH_REPLACE_CALL_ID);
    assert!(provider_content.len() <= 60 * 1024);
    let result: Value = serde_json::from_str(provider_content).unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["path"], PATCH_REPLACE_PATH);
    assert_eq!(result["filesModified"], json!([PATCH_REPLACE_PATH]));
    assert_eq!(result["replacements"], 1);
    let diff = result["diff"].as_str().unwrap();
    assert!(diff.contains(&format!("--- a/{PATCH_REPLACE_PATH}")));
    assert!(diff.contains(&format!("+++ b/{PATCH_REPLACE_PATH}")));
    assert!(diff.contains(PATCH_REPLACE_OLD));
    assert!(diff.contains(PATCH_REPLACE_NEW));
    let root = workspace.path().to_string_lossy();
    assert!(!provider_content.contains(root.as_ref()));
    assert!(!provider_content.contains(&root.replace('\\', "\\\\")));
}

#[tokio::test]
async fn workspace_patch_v4a_once_creates_file_after_approval_without_public_patch_text() {
    let harness = Harness::new().await;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("generated")).unwrap();
    let input_summary = "Apply V4A patch (1 operation)";
    let pending = start_patch_run_waiting_for_approval(
        &harness,
        workspace.path(),
        "v4a-once",
        "use workspace patch v4a once",
        PATCH_V4A_CALL_ID,
        input_summary,
    )
    .await;

    assert!(!workspace.path().join(PATCH_V4A_PATH).exists());
    let waiting_messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_public_patch_state_redacted(
        &[],
        &pending.waiting,
        &waiting_messages,
        workspace.path(),
        &[PATCH_V4A_TEXT, PATCH_V4A_CONTENT],
    );

    let approved = post_approval(
        &harness.app,
        &pending.run_id,
        &pending.approval_id,
        "once",
        None,
    )
    .await;
    assert_eq!(approved.status(), StatusCode::OK);
    assert_eq!(json_body(approved).await, json!({"accepted": true}));
    let completed = wait_for_run_status(&harness.app, &pending.run_id, "completed").await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_V4A_PATH)).unwrap(),
        PATCH_V4A_CONTENT
    );

    let events = collect_events(events_request(&harness.app, &pending.run_id, None).await).await;
    assert_patch_once_journal(&events, &pending, PATCH_V4A_CALL_ID, input_summary);
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_completed_patch_message(&messages, PATCH_V4A_CALL_ID);
    assert_public_patch_state_redacted(
        &events,
        &completed,
        &messages,
        workspace.path(),
        &[PATCH_V4A_TEXT, PATCH_V4A_CONTENT],
    );

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    let provider_content = provider_tool_content(&requests[1], PATCH_V4A_CALL_ID);
    assert!(provider_content.len() <= 60 * 1024);
    let result: Value = serde_json::from_str(provider_content).unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["filesCreated"], json!([PATCH_V4A_PATH]));
    let diff = result["diff"].as_str().unwrap();
    assert!(diff.contains(&format!("+++ b/{PATCH_V4A_PATH}")));
    assert!(diff.contains(PATCH_V4A_CONTENT));
    let root = workspace.path().to_string_lossy();
    assert!(!provider_content.contains(root.as_ref()));
    assert!(!provider_content.contains(&root.replace('\\', "\\\\")));
}

#[tokio::test]
async fn workspace_patch_deny_is_idempotent_and_continues_provider_without_side_effects() {
    let harness = Harness::new().await;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    let original = format!("before\n{PATCH_REPLACE_OLD}\nafter\n");
    std::fs::write(workspace.path().join(PATCH_REPLACE_PATH), &original).unwrap();
    let input_summary = format!("Patch {PATCH_REPLACE_PATH} (+1/-1 lines, one)");
    let pending = start_patch_run_waiting_for_approval(
        &harness,
        workspace.path(),
        "deny",
        "use workspace patch replace deny",
        PATCH_REPLACE_CALL_ID,
        &input_summary,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        original
    );
    let waiting_messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_public_patch_state_redacted(
        &[],
        &pending.waiting,
        &waiting_messages,
        workspace.path(),
        &[PATCH_REPLACE_OLD, PATCH_REPLACE_NEW],
    );

    let denied = post_approval(
        &harness.app,
        &pending.run_id,
        &pending.approval_id,
        "deny",
        Some("not for this patch run"),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(json_body(denied).await, json!({"accepted": true}));
    let completed = wait_for_run_status(&harness.app, &pending.run_id, "completed").await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        original
    );

    let events = collect_events(events_request(&harness.app, &pending.run_id, None).await).await;
    assert_event_journal(&events, &pending.run_id, &pending.session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.failed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "patch");
    assert_eq!(events[3].data["data"]["inputSummary"], input_summary);
    assert_eq!(events[5].data["data"]["decision"], "deny");
    assert_eq!(events[5].data["data"]["resolvedBy"], "user");
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "tool_execution_denied"
    );
    assert_eq!(events[7].data["data"]["delta"], "Patch handled");
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    let call = &messages["items"][1]["toolCalls"][0];
    assert_eq!(call["callId"], PATCH_REPLACE_CALL_ID);
    assert_eq!(call["name"], "patch");
    assert_eq!(call["status"], "failed");
    assert_eq!(call["inputSummary"], input_summary);
    assert_eq!(call["resultSummary"], "Tool execution denied");
    assert_public_patch_state_redacted(
        &events,
        &completed,
        &messages,
        workspace.path(),
        &[PATCH_REPLACE_OLD, PATCH_REPLACE_NEW],
    );

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        provider_tool_content(&requests[1], PATCH_REPLACE_CALL_ID),
        "Tool execution denied before side effects began"
    );
    let sequence = completed["lastSequence"].as_u64().unwrap();
    let replay = post_approval(
        &harness.app,
        &pending.run_id,
        &pending.approval_id,
        "deny",
        Some("not for this patch run"),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await, json!({"accepted": true}));
    assert_eq!(
        get_json(&harness.app, &format!("/api/v1/runs/{}", pending.run_id)).await["lastSequence"],
        sequence
    );
    let conflict = post_approval(
        &harness.app,
        &pending.run_id,
        &pending.approval_id,
        "deny",
        Some("different reason"),
    )
    .await;
    assert_problem(conflict, StatusCode::CONFLICT, "approval_decision_conflict").await;
    assert_eq!(harness.provider.call_count(), 2);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        original
    );
}

#[tokio::test]
async fn workspace_patch_cancel_first_rejects_late_approval_without_provider_continuation() {
    let harness = Harness::new().await;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    let original = format!("before\n{PATCH_REPLACE_OLD}\nafter\n");
    std::fs::write(workspace.path().join(PATCH_REPLACE_PATH), &original).unwrap();
    let input_summary = format!("Patch {PATCH_REPLACE_PATH} (+1/-1 lines, one)");
    let pending = start_patch_run_waiting_for_approval(
        &harness,
        workspace.path(),
        "cancel",
        "use workspace patch replace cancel",
        PATCH_REPLACE_CALL_ID,
        &input_summary,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        original
    );
    let waiting_messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_public_patch_state_redacted(
        &[],
        &pending.waiting,
        &waiting_messages,
        workspace.path(),
        &[PATCH_REPLACE_OLD, PATCH_REPLACE_NEW],
    );

    let cancel = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{}/cancel", pending.run_id),
        Body::empty(),
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);
    let _ = json_body(cancel).await;
    let cancelled = wait_for_run_status(&harness.app, &pending.run_id, "cancelled").await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(PATCH_REPLACE_PATH)).unwrap(),
        original
    );
    let sequence = cancelled["lastSequence"].as_u64().unwrap();

    let late = post_approval(
        &harness.app,
        &pending.run_id,
        &pending.approval_id,
        "once",
        None,
    )
    .await;
    assert_problem(late, StatusCode::CONFLICT, "approval_no_longer_pending").await;
    assert_eq!(
        get_json(&harness.app, &format!("/api/v1/runs/{}", pending.run_id)).await["lastSequence"],
        sequence
    );

    let events = collect_events(events_request(&harness.app, &pending.run_id, None).await).await;
    assert_event_journal(&events, &pending.run_id, &pending.session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.failed",
            "run.cancelled",
        ]
    );
    assert_eq!(events[3].data["data"]["name"], "patch");
    assert_eq!(events[3].data["data"]["inputSummary"], input_summary);
    assert_eq!(events[5].data["data"]["decision"], "deny");
    assert_eq!(events[5].data["data"]["resolvedBy"], "cancellation");
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "tool_execution_cancelled"
    );
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{}/messages", pending.session_id),
    )
    .await;
    assert_public_patch_state_redacted(
        &events,
        &cancelled,
        &messages,
        workspace.path(),
        &[PATCH_REPLACE_OLD, PATCH_REPLACE_NEW],
    );
    assert_eq!(harness.provider.call_count(), 1);
}

#[tokio::test]
async fn active_run_discovery_is_authenticated_strict_and_session_filtered() {
    let harness = Harness::new().await;
    let first_session = create_session(&harness.app, "active-discovery-http-session-1").await;
    let second_session = create_session(&harness.app, "active-discovery-http-session-2").await;
    let first = post_run(
        &harness.app,
        &first_session,
        "active-discovery-http-run-1",
        &run_request("active-discovery-request-1", "slow response first"),
    )
    .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first = json_body(first).await;
    let second = post_run(
        &harness.app,
        &second_session,
        "active-discovery-http-run-2",
        &run_request("active-discovery-request-2", "slow response second"),
    )
    .await;
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    let second = json_body(second).await;

    let path = "/api/v1/runs?profileId=default&state=active";
    let unauthorized = harness
        .app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = authorized_request(&harness.app, Method::GET, path, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = json_body(response).await;
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["run"]["id"], first["run"]["id"]);
    assert_eq!(items[1]["run"]["id"], second["run"]["id"]);
    assert_eq!(items[0]["queueItemId"], Value::Null);
    assert_eq!(items[0]["userMessage"], first["userMessage"]);
    assert_eq!(items[0]["run"]["profileId"], "default");
    assert_eq!(items[0]["userMessage"]["sessionId"], first_session);
    assert_eq!(items[0].as_object().unwrap().len(), 4);
    let session = get_json(&harness.app, &format!("/api/v1/sessions/{first_session}")).await;
    assert_eq!(items[0]["sessionRevision"], session["revision"]);

    let filtered_path =
        format!("/api/v1/runs?profileId=default&state=active&sessionId={second_session}");
    let filtered =
        authorized_request(&harness.app, Method::GET, &filtered_path, Body::empty()).await;
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered = json_body(filtered).await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["items"][0]["run"]["id"], second["run"]["id"]);

    for invalid_path in [
        "/api/v1/runs?profileId=default",
        "/api/v1/runs?profileId=default&state=terminal",
        "/api/v1/runs?profileId=default&state=active&unknown=true",
        "/api/v1/runs?profileId=default&state=active&sessionId=not-a-session",
    ] {
        let response =
            authorized_request(&harness.app, Method::GET, invalid_path, Body::empty()).await;
        assert_problem(response, StatusCode::BAD_REQUEST, "validation_failed").await;
    }
    let missing_profile = authorized_request(
        &harness.app,
        Method::GET,
        "/api/v1/runs?profileId=missing&state=active",
        Body::empty(),
    )
    .await;
    assert_eq!(missing_profile.status(), StatusCode::NOT_FOUND);

    for run_id in [first["run"]["id"].as_str(), second["run"]["id"].as_str()]
        .into_iter()
        .flatten()
    {
        let cancelled = authorized_request(
            &harness.app,
            Method::POST,
            &format!("/api/v1/runs/{run_id}/cancel"),
            Body::empty(),
        )
        .await;
        assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    }
}

#[tokio::test]
async fn active_session_queues_a_second_run_and_cancellation_advances_fifo() {
    let harness = Harness::new().await;
    let session_id = create_session(&harness.app, "cancelled-run-session").await;
    let slow_request = run_request("request-slow", "slow response");
    let accepted = post_run(&harness.app, &session_id, "slow-run-key", &slow_request).await;
    let accepted_status = accepted.status();
    let accepted = json_body(accepted).await;
    assert_eq!(
        accepted_status,
        StatusCode::ACCEPTED,
        "unexpected response: {accepted}"
    );
    let run_id = accepted["run"]["id"].as_str().unwrap().to_owned();

    harness.provider.wait_for_calls(1).await;
    let second_request = run_request("request-second", "second response");
    let queued = post_run(&harness.app, &session_id, "second-run-key", &second_request).await;
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued = json_body(queued).await;
    assert_eq!(queued["disposition"], "queued");
    assert_eq!(queued["run"]["status"], "queued");
    assert!(queued["queueItemId"].as_str().is_some());
    let queued_run_id = queued["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(harness.provider.call_count(), 1);

    let cancel = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{run_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);
    let cancel = json_body(cancel).await;
    assert!(matches!(
        cancel["status"].as_str(),
        Some("cancelling" | "cancelled")
    ));

    let cancelled = wait_for_run_status(&harness.app, &run_id, "cancelled").await;
    assert_eq!(cancelled["error"], Value::Null);
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    let names = event_names(&events);
    assert_eq!(names.first(), Some(&"run.started"));
    assert_eq!(names.last(), Some(&"run.cancelled"));
    assert!(!names.contains(&"run.completed"));

    let completed = wait_for_run_status(&harness.app, &queued_run_id, "completed").await;
    assert_eq!(completed["error"], Value::Null);
    let queued_events =
        collect_events(events_request(&harness.app, &queued_run_id, None).await).await;
    assert_event_journal(&queued_events, &queued_run_id, &session_id);
    let queued_names = event_names(&queued_events);
    assert_eq!(queued_names.first(), Some(&"run.queued"));
    assert_eq!(queued_names.get(1), Some(&"run.started"));
    assert_eq!(queued_names.last(), Some(&"run.completed"));

    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert_eq!(messages["items"].as_array().unwrap().len(), 3);
    assert_eq!(messages["items"][0]["parts"][0]["text"], "slow response");
    assert_eq!(messages["items"][1]["parts"][0]["text"], "second response");
    assert_eq!(harness.provider.call_count(), 2);
}

#[tokio::test]
async fn queued_run_cancel_is_persistent_terminal_and_never_calls_the_provider() {
    let harness = Harness::new().await;
    let session_id = create_session(&harness.app, "queued-cancel-session").await;
    let running = post_run(
        &harness.app,
        &session_id,
        "queued-cancel-running-key",
        &run_request("queued-cancel-running", "slow response"),
    )
    .await;
    assert_eq!(running.status(), StatusCode::ACCEPTED);
    let running_id = json_body(running).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness.provider.wait_for_calls(1).await;
    let queued = post_run(
        &harness.app,
        &session_id,
        "queued-cancel-item-key",
        &run_request("queued-cancel-item", "must not execute"),
    )
    .await;
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued = json_body(queued).await;
    assert_eq!(queued["run"]["status"], "queued");
    let queued_id = queued["run"]["id"].as_str().unwrap();
    let cancelled = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{queued_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    let cancelled = json_body(cancelled).await;
    assert_eq!(cancelled["status"], "cancelled");
    let events = collect_events(events_request(&harness.app, queued_id, None).await).await;
    assert_eq!(event_names(&events), ["run.queued", "run.cancelled"]);
    assert_eq!(harness.provider.call_count(), 1);

    let cleanup = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{running_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cleanup.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn expired_runtime_lease_takeover_fences_the_old_owner_and_resumes_the_queue() {
    let harness = Harness::new().await;
    let session_id = create_session(&harness.app, "queued-restart-session").await;
    let running = post_run(
        &harness.app,
        &session_id,
        "queued-restart-running-key",
        &run_request("queued-restart-running", "slow response"),
    )
    .await;
    assert_eq!(running.status(), StatusCode::ACCEPTED);
    let running_id = json_body(running).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness.provider.wait_for_calls(1).await;
    let queued = post_run(
        &harness.app,
        &session_id,
        "queued-restart-next-key",
        &run_request("queued-restart-next", "resume after restart"),
    )
    .await;
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued = json_body(queued).await;
    assert_eq!(queued["run"]["status"], "queued");
    let queued_id = queued["run"]["id"].as_str().unwrap().to_owned();

    let restarted = harness.restarted_app();
    let capabilities = get_json(&restarted, "/api/v1/capabilities").await;
    assert_eq!(capabilities["engine"]["available"], true);
    assert_eq!(capabilities["extensions"]["runQueue"], true);
    let stale_owner = post_run(
        &harness.app,
        &session_id,
        "queued-restart-stale-owner-key",
        &run_request("queued-restart-stale-owner", "must be fenced"),
    )
    .await;
    assert_problem(stale_owner, StatusCode::BAD_GATEWAY, "engine_unavailable").await;
    let failed = get_json(&restarted, &format!("/api/v1/runs/{running_id}")).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["code"], "engine_unavailable");
    let completed = wait_for_run_status(&restarted, &queued_id, "completed").await;
    assert_eq!(completed["error"], Value::Null);
    let events = collect_events(events_request(&restarted, &queued_id, None).await).await;
    assert_eq!(event_names(&events).first(), Some(&"run.queued"));
    assert_eq!(event_names(&events).get(1), Some(&"run.started"));
    assert_eq!(event_names(&events).last(), Some(&"run.completed"));
    assert_eq!(harness.provider.call_count(), 2);
}

#[tokio::test]
async fn unexpired_runtime_lease_rejects_a_contender_without_fencing_the_owner() {
    let harness = Harness::new().await;
    let session_id = create_session(&harness.app, "runtime-contender-session").await;
    let running = post_run(
        &harness.app,
        &session_id,
        "runtime-contender-running-key",
        &run_request("runtime-contender-running", "slow response"),
    )
    .await;
    assert_eq!(running.status(), StatusCode::ACCEPTED);
    let running_id = json_body(running).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness.provider.wait_for_calls(1).await;

    let contender = harness.contending_app();
    let capabilities = get_json(&contender, "/api/v1/capabilities").await;
    assert_eq!(capabilities["engine"]["available"], false);
    assert_eq!(capabilities["extensions"]["runQueue"], false);
    let rejected = post_run(
        &contender,
        &session_id,
        "runtime-contender-rejected-key",
        &run_request("runtime-contender-rejected", "must not enqueue"),
    )
    .await;
    assert_problem(rejected, StatusCode::BAD_GATEWAY, "engine_unavailable").await;
    assert_eq!(harness.provider.call_count(), 1);

    let cleanup = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{running_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cleanup.status(), StatusCode::ACCEPTED);
    let cancelled = wait_for_run_status(&harness.app, &running_id, "cancelled").await;
    assert_eq!(cancelled["status"], "cancelled");
}

#[tokio::test]
async fn waiting_clarification_fails_closed_on_restart_and_rejects_a_late_answer() {
    let (harness, session_id, run_id, request_id) = std::thread::spawn(|| {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async {
            let harness = Harness::new().await;
            harness.enable_toolset("clarify").await;
            let session_id = create_session(&harness.app, "clarification-restart-session").await;
            let accepted = post_run(
                &harness.app,
                &session_id,
                "clarification-restart-run",
                &run_request(
                    "request-clarification-restart",
                    "use clarification freeform restart",
                ),
            )
            .await;
            assert_eq!(accepted.status(), StatusCode::ACCEPTED);
            let run_id = json_body(accepted).await["run"]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let waiting = wait_for_pending_clarification(&harness.app, &run_id).await;
            let request_id = waiting["pendingAction"]["requestId"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(harness.provider.call_count(), 1);
            (harness, session_id, run_id, request_id)
        });
        drop(runtime);
        result
    })
    .join()
    .expect("the pre-restart clarification runtime should exit cleanly");

    let restarted = harness.restarted_app();
    let failed = get_json(&restarted, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["pendingAction"], Value::Null);
    assert_eq!(failed["error"]["code"], "engine_unavailable");
    let events = collect_events(events_request(&restarted, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "clarification.required",
            "clarification.resolved",
            "tool.failed",
            "run.failed",
        ]
    );
    assert_eq!(
        events[5].data["data"],
        json!({
            "requestId": request_id,
            "resolvedBy": "failure",
        })
    );
    assert_eq!(
        events[6].data["data"]["error"]["code"],
        "clarification_interrupted"
    );

    let late = post_clarification(
        &restarted,
        &run_id,
        &request_id,
        CLARIFICATION_PRIVATE_ANSWER,
    )
    .await;
    assert_problem(
        late,
        StatusCode::CONFLICT,
        "clarification_no_longer_pending",
    )
    .await;
    assert!(
        !events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>()
            .contains("PRIVATE_CLARIFICATION_ANSWER_DO_NOT_EXPOSE")
    );
    assert_eq!(harness.provider.call_count(), 1);
}

#[tokio::test]
async fn interrupted_run_fails_on_restart_and_its_terminal_journal_remains_replayable() {
    let (harness, session_id, run_id) = std::thread::spawn(|| {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async {
            let harness = Harness::new().await;
            let session_id = create_session(&harness.app, "restart-run-session").await;
            let request = run_request("request-restart", "slow response");
            let accepted = post_run(&harness.app, &session_id, "restart-run-key", &request).await;
            assert_eq!(accepted.status(), StatusCode::ACCEPTED);
            let accepted = json_body(accepted).await;
            let run_id = accepted["run"]["id"].as_str().unwrap().to_owned();

            harness.provider.wait_for_calls(1).await;
            let running = wait_for_run_sequence(&harness.app, &run_id, 3).await;
            assert_eq!(running["status"], "running");
            assert_eq!(running["lastSequence"], 3);
            (harness, session_id, run_id)
        });
        drop(runtime);
        result
    })
    .join()
    .expect("the pre-restart backend runtime should exit cleanly");

    let restarted = harness.restarted_app();
    let failed = get_json(&restarted, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["lastSequence"], 4);
    assert_eq!(failed["error"]["code"], "engine_unavailable");
    assert_eq!(failed["error"]["title"], "Run interrupted");
    assert_eq!(failed["error"]["retryable"], true);
    assert_eq!(
        failed["error"]["instance"],
        format!("/api/v1/runs/{run_id}")
    );
    assert_eq!(failed["error"]["requestId"], format!("run:{run_id}"));

    let events = collect_events(events_request(&restarted, &run_id, None).await).await;
    assert_event_journal(&events, &run_id, &session_id);
    assert_eq!(
        event_names(&events),
        [
            "run.started",
            "message.started",
            "message.delta",
            "run.failed"
        ]
    );
    assert_eq!(events[2].data["data"]["delta"], "Partial");
    assert_eq!(events[3].data["data"]["error"], failed["error"]);

    let restarted_again = harness.restarted_app();
    let replayed =
        collect_events(events_request(&restarted_again, &run_id, Some(&events[1].id)).await).await;
    assert_eq!(replayed, events[2..]);

    let unchanged = get_json(&restarted_again, &format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(unchanged["status"], "failed");
    assert_eq!(unchanged["lastSequence"], 4);
}

struct PendingPatchRun {
    session_id: String,
    run_id: String,
    approval_id: String,
    waiting: Value,
}

async fn start_patch_run_waiting_for_approval(
    harness: &Harness,
    workspace: &Path,
    case_id: &str,
    prompt: &str,
    expected_call_id: &str,
    expected_input_summary: &str,
) -> PendingPatchRun {
    harness.enable_toolset("file").await;
    let workspace_id = register_workspace(&harness.app, workspace).await;
    let session_key = format!("patch-{case_id}-session");
    let session_id = create_session(&harness.app, &session_key).await;
    let mut request = run_request(&format!("request-patch-{case_id}"), prompt);
    request["workspaceId"] = json!(workspace_id);
    let accepted = post_run(
        &harness.app,
        &session_id,
        &format!("patch-{case_id}-run"),
        &request,
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = wait_for_pending_approval(&harness.app, &run_id).await;
    let pending = &waiting["pendingAction"];
    assert_eq!(waiting["status"], "waitingApproval");
    assert_eq!(pending["kind"], "approval");
    assert_eq!(pending["callId"], expected_call_id);
    assert_eq!(pending["toolName"], "patch");
    assert_eq!(pending["choices"], json!(["once", "deny"]));
    assert_eq!(pending["inputSummary"], expected_input_summary);
    assert_eq!(harness.provider.call_count(), 1);
    let approval_id = pending["approvalId"].as_str().unwrap().to_owned();
    PendingPatchRun {
        session_id,
        run_id,
        approval_id,
        waiting,
    }
}

fn assert_patch_once_journal(
    events: &[CapturedEvent],
    pending: &PendingPatchRun,
    call_id: &str,
    input_summary: &str,
) {
    assert_event_journal(events, &pending.run_id, &pending.session_id);
    assert_eq!(
        event_names(events),
        [
            "run.started",
            "message.started",
            "usage.updated",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "message.delta",
            "usage.updated",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[3].data["data"]["callId"], call_id);
    assert_eq!(events[3].data["data"]["name"], "patch");
    assert_eq!(events[3].data["data"]["inputSummary"], input_summary);
    assert_eq!(events[4].data["data"]["approvalId"], pending.approval_id);
    assert_eq!(events[4].data["data"]["inputSummary"], input_summary);
    assert_eq!(events[5].data["data"]["decision"], "once");
    assert_eq!(events[5].data["data"]["resolvedBy"], "user");
    assert_eq!(events[6].data["data"]["callId"], call_id);
    assert_eq!(
        events[6].data["data"]["resultSummary"],
        "Applied 1 patch operation(s)"
    );
    assert_eq!(events[7].data["data"]["delta"], "Patch handled");
}

fn assert_completed_patch_message(messages: &Value, call_id: &str) {
    let assistant = &messages["items"][1];
    let call = &assistant["toolCalls"][0];
    assert_eq!(call["callId"], call_id);
    assert_eq!(call["name"], "patch");
    assert_eq!(call["status"], "completed");
    assert_eq!(call["inputSummary"], "Apply patch (1 affected path(s))");
    assert_eq!(call["resultSummary"], "Applied 1 patch operation(s)");
}

fn provider_tool_content<'a>(request: &'a Value, call_id: &str) -> &'a str {
    let messages = request["messages"].as_array().unwrap();
    let tool = messages.last().unwrap();
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], call_id);
    tool["content"].as_str().unwrap()
}

fn compile_mcp_stdio_fixture(home: &TempDir) -> PathBuf {
    let source = home.path().join("run_mcp_stdio_fixture.rs");
    let executable = home.path().join(if cfg!(windows) {
        "run_mcp_stdio_fixture.exe"
    } else {
        "run_mcp_stdio_fixture"
    });
    std::fs::write(
        &source,
        r#"
use std::io::{self, BufRead, Write};

fn request_id(line: &str) -> &str {
    line.split("\"id\":").nth(1).unwrap().split(|ch| ch == ',' || ch == '}').next().unwrap()
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line.contains("\"method\":\"initialize\"") {
            writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{}}}}}}", request_id(&line)).unwrap();
        } else if line.contains("\"method\":\"tools/list\"") {
            writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{{\"name\":\"echo\",\"description\":\"Echo a value\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{\"text\":{{\"type\":\"string\"}}}},\"required\":[\"text\"],\"additionalProperties\":false}}}}]}}}}", request_id(&line)).unwrap();
        } else if line.contains("\"method\":\"tools/call\"") {
            writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"MCP_PRIVATE_RESULT_DO_NOT_EXPOSE\"}}],\"isError\":false}}}}", request_id(&line)).unwrap();
        }
        stdout.flush().unwrap();
    }
}
"#,
    )
    .unwrap();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = std::process::Command::new(rustc)
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .unwrap();
    assert!(status.success());
    executable
}

fn run_request(client_request_id: &str, text: &str) -> Value {
    json!({
        "clientRequestId": client_request_id,
        "message": {"text": text, "fileIds": []},
        "modelOverride": null,
        "reasoningEffort": null
    })
}

async fn create_session(app: &Router, idempotency_key: &str) -> String {
    let response = authorized_request_builder(
        app,
        Request::post("/api/v1/sessions")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", idempotency_key)
            .body(Body::from(
                json!({"profileId": "default", "title": "Run HTTP test"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["id"].as_str().unwrap().to_owned()
}

async fn register_workspace(app: &Router, path: &Path) -> String {
    let response = authorized_request_builder(
        app,
        Request::post("/api/v1/profiles/default/workspaces")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "workspace-file-tools")
            .body(Body::from(
                json!({"path": path.to_string_lossy()}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["id"].as_str().unwrap().to_owned()
}

async fn wait_for_pending_approval(app: &Router, run_id: &str) -> Value {
    timeout(WAIT_TIMEOUT, async {
        loop {
            let run = get_json(app, &format!("/api/v1/runs/{run_id}")).await;
            if run["status"] == "waitingApproval" && run["pendingAction"]["kind"] == "approval" {
                return run;
            }
            assert_ne!(run["status"], "failed", "run failed before approval: {run}");
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run should expose its pending approval")
}

async fn wait_for_pending_clarification(app: &Router, run_id: &str) -> Value {
    timeout(WAIT_TIMEOUT, async {
        loop {
            let run = get_json(app, &format!("/api/v1/runs/{run_id}")).await;
            if run["status"] == "waitingClarification"
                && run["pendingAction"]["kind"] == "clarification"
            {
                return run;
            }
            assert_ne!(
                run["status"], "failed",
                "run failed before clarification: {run}"
            );
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run should expose its pending clarification")
}

async fn post_approval(
    app: &Router,
    run_id: &str,
    approval_id: &str,
    decision: &str,
    reason: Option<&str>,
) -> Response<Body> {
    authorized_request_builder(
        app,
        Request::post(format!("/api/v1/runs/{run_id}/approvals/{approval_id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"decision": decision, "reason": reason}).to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn post_clarification(
    app: &Router,
    run_id: &str,
    request_id: &str,
    answer: &str,
) -> Response<Body> {
    authorized_request_builder(
        app,
        Request::post(format!("/api/v1/runs/{run_id}/clarifications/{request_id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"answer": answer}).to_string()))
            .unwrap(),
    )
    .await
}

async fn post_run(
    app: &Router,
    session_id: &str,
    idempotency_key: &str,
    body: &Value,
) -> Response<Body> {
    authorized_request_builder(
        app,
        Request::post(format!("/api/v1/sessions/{session_id}/runs"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", idempotency_key)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn events_request(app: &Router, run_id: &str, last_event_id: Option<&str>) -> Response<Body> {
    let mut builder = Request::get(format!("/api/v1/runs/{run_id}/events"));
    if let Some(last_event_id) = last_event_id {
        builder = builder.header("last-event-id", last_event_id);
    }
    authorized_request_builder(app, builder.body(Body::empty()).unwrap()).await
}

async fn wait_for_run_status(app: &Router, run_id: &str, expected: &str) -> Value {
    let result = timeout(WAIT_TIMEOUT, async {
        loop {
            let run = get_json(app, &format!("/api/v1/runs/{run_id}")).await;
            if run["status"] == expected {
                return run;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    match result {
        Ok(run) => run,
        Err(_) => {
            let final_run = get_json(app, &format!("/api/v1/runs/{run_id}")).await;
            panic!("the run should reach {expected}; final state: {final_run}");
        }
    }
}

async fn wait_for_run_sequence(app: &Router, run_id: &str, expected: u64) -> Value {
    timeout(WAIT_TIMEOUT, async {
        loop {
            let run = get_json(app, &format!("/api/v1/runs/{run_id}")).await;
            if run["lastSequence"]
                .as_u64()
                .is_some_and(|value| value >= expected)
            {
                return run;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run journal should reach the expected sequence")
}

async fn get_json(app: &Router, path: &str) -> Value {
    let response = authorized_request(app, Method::GET, path, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn authorized_request(
    app: &Router,
    method: Method,
    path: &str,
    body: Body,
) -> Response<Body> {
    authorized_request_builder(
        app,
        Request::builder()
            .method(method)
            .uri(path)
            .body(body)
            .unwrap(),
    )
    .await
}

async fn authorized_request_builder(app: &Router, mut request: Request<Body>) -> Response<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    app.clone().oneshot(request).await.unwrap()
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let body = json_body(response).await;
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code);
}

#[derive(Clone, Debug, PartialEq)]
struct CapturedEvent {
    id: String,
    name: String,
    data: Value,
}

async fn collect_events(response: Response<Body>) -> Vec<CapturedEvent> {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let bytes = timeout(WAIT_TIMEOUT, response.into_body().collect())
        .await
        .expect("a terminal run should close its SSE response")
        .unwrap()
        .to_bytes();
    parse_events(&bytes)
}

fn parse_events(bytes: &[u8]) -> Vec<CapturedEvent> {
    let body = String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace("\r\n", "\n");
    body.split("\n\n")
        .filter_map(|block| {
            let mut id = None;
            let mut name = None;
            let mut data = Vec::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("id:") {
                    id = Some(value.trim_start().to_owned());
                } else if let Some(value) = line.strip_prefix("event:") {
                    name = Some(value.trim_start().to_owned());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push(value.trim_start());
                }
            }
            let (id, name) = id.zip(name)?;
            Some(CapturedEvent {
                id,
                name,
                data: serde_json::from_str(&data.join("\n")).unwrap(),
            })
        })
        .collect()
}

fn assert_event_journal(events: &[CapturedEvent], run_id: &str, session_id: &str) {
    assert!(!events.is_empty());
    for (index, event) in events.iter().enumerate() {
        let sequence = index + 1;
        assert_eq!(event.id, format!("{run_id}:{sequence}"));
        assert_eq!(event.data["schemaVersion"], 1);
        assert_eq!(event.data["sequence"], sequence as u64);
        assert_eq!(event.data["runId"], run_id);
        assert_eq!(event.data["sessionId"], session_id);
        assert!(event.data["occurredAt"].as_str().is_some());
    }
}

fn event_names(events: &[CapturedEvent]) -> Vec<&str> {
    events.iter().map(|event| event.name.as_str()).collect()
}

fn assert_public_write_state_redacted(events: &[CapturedEvent], run: &Value, workspace: &Path) {
    let public = format!(
        "{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        run
    );
    assert!(!public.contains(WRITE_CONTENT));
    let root = workspace.to_string_lossy();
    assert!(!public.contains(root.as_ref()));
    assert!(!public.contains(&root.replace('\\', "\\\\")));
}

fn assert_public_terminal_state_redacted(
    events: &[CapturedEvent],
    state: &Value,
    workspace: &Path,
) {
    let public = format!(
        "{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        state,
    );
    for sensitive in [
        TERMINAL_OUTPUT,
        TERMINAL_FILE_CONTENT.trim_end(),
        TERMINAL_PATH,
    ] {
        assert!(
            !public.contains(sensitive),
            "public terminal state leaked command or output text"
        );
    }
    let root = workspace.to_string_lossy();
    assert!(!public.contains(root.as_ref()));
    assert!(!public.contains(&root.replace('\\', "\\\\")));
}

fn assert_public_process_state_redacted(
    events: &[CapturedEvent],
    run: &Value,
    messages: &Value,
    workspace: &Path,
) {
    let public = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        run,
        messages,
    );
    assert!(!public.contains(BACKGROUND_OUTPUT));
    assert!(!public.contains("while true"));
    let root = workspace.to_string_lossy();
    assert!(!public.contains(root.as_ref()));
    assert!(!public.contains(&root.replace('\\', "\\\\")));
}

fn assert_public_async_delivery_redacted(events: &[CapturedEvent], run: &Value, workspace: &Path) {
    let public = format!(
        "{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        run,
    );
    for sensitive in [
        ASYNC_PRIVATE_OUTPUT,
        ASYNC_WATCH_PATTERN,
        "printf",
        "while true",
    ] {
        assert!(
            !public.contains(sensitive),
            "public async delivery state leaked private process data"
        );
    }
    let root = workspace.to_string_lossy();
    assert!(!public.contains(root.as_ref()));
    assert!(!public.contains(&root.replace('\\', "\\\\")));
}

fn assert_public_patch_state_redacted(
    events: &[CapturedEvent],
    run: &Value,
    messages: &Value,
    workspace: &Path,
    sensitive_values: &[&str],
) {
    let public = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        run,
        messages,
    );
    for sensitive in sensitive_values {
        assert!(
            !public.contains(sensitive),
            "public patch state leaked sensitive text"
        );
        let escaped = serde_json::to_string(sensitive).unwrap();
        let escaped = escaped
            .strip_prefix('"')
            .unwrap()
            .strip_suffix('"')
            .unwrap();
        assert!(
            !public.contains(escaped),
            "public patch state leaked JSON-escaped sensitive text"
        );
    }
    let root = workspace.to_string_lossy();
    assert!(!public.contains(root.as_ref()));
    assert!(!public.contains(&root.replace('\\', "\\\\")));

    let mut summaries = events
        .iter()
        .flat_map(|event| ["inputSummary", "resultSummary"].map(|key| &event.data["data"][key]))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if let Some(summary) = run["pendingAction"]["inputSummary"].as_str() {
        summaries.push(summary);
    }
    for message in messages["items"].as_array().into_iter().flatten() {
        for call in message["toolCalls"].as_array().into_iter().flatten() {
            summaries.extend(
                ["inputSummary", "resultSummary"]
                    .into_iter()
                    .filter_map(|key| call[key].as_str()),
            );
        }
    }
    for summary in summaries {
        for sensitive in sensitive_values {
            assert!(!summary.contains(sensitive));
        }
        assert!(!summary.contains(root.as_ref()));
    }
}
