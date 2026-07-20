use std::{
    convert::Infallible,
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_stream::stream;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{Method, Request, Response, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use http_body_util::BodyExt;
use secrecy::SecretString;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::Notify,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tower::ServiceExt;
use url::Url;

const TOKEN: &str = "01234567890123456789012345678901";
const TAVILY_KEY: &str = "tvly-private-key-do-not-expose";
const SEARCH_CALL_ID: &str = "call-web-search-1";
const EXTRACT_CALL_ID: &str = "call-web-extract-1";
const SEARCH_QUERY: &str = "SENSITIVE_WEB_QUERY_DO_NOT_EXPOSE";
const SLOW_SEARCH_QUERY: &str = "SLOW_WEB_QUERY_DO_NOT_EXPOSE";
const SEARCH_URL: &str =
    "https://1.1.1.1/private-search-result?term=SENSITIVE_RESULT_URL_DO_NOT_EXPOSE";
const EXTRACT_URL: &str =
    "https://1.1.1.1/private-extract-page?view=SENSITIVE_EXTRACT_URL_DO_NOT_EXPOSE";
const RAW_BODY: &str = "PRIVATE_TAVILY_RAW_BODY_DO_NOT_EXPOSE";
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);

struct Harness {
    app: Router,
    home: TempDir,
    llm: Arc<MockLlm>,
    tavily: Arc<MockTavily>,
    llm_server: JoinHandle<()>,
    tavily_server: JoinHandle<()>,
}

impl Harness {
    async fn new() -> Self {
        let llm = Arc::new(MockLlm::default());
        let llm_app = Router::new()
            .route("/v1/chat/completions", post(mock_chat_completions))
            .with_state(llm.clone());
        let llm_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm_address = llm_listener.local_addr().unwrap();
        let llm_server = tokio::spawn(async move {
            axum::serve(llm_listener, llm_app).await.unwrap();
        });

        let tavily = Arc::new(MockTavily::default());
        let tavily_app = Router::new()
            .route("/search", post(mock_tavily_search))
            .route("/extract", post(mock_tavily_extract))
            .with_state(tavily.clone());
        let tavily_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tavily_address = tavily_listener.local_addr().unwrap();
        let tavily_server = tokio::spawn(async move {
            axum::serve(tavily_listener, tavily_app).await.unwrap();
        });

        let home = tempfile::tempdir().unwrap();
        let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
        profiles
            .put_secret(
                "default",
                "TAVILY_API_KEY",
                &SecretString::from(TAVILY_KEY.to_owned()),
            )
            .unwrap();
        let config = AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        )
        .with_web_base_url_for_tests(Url::parse(&format!("http://{tavily_address}/")).unwrap());
        let app = build_router(config);
        let harness = Self {
            app,
            home,
            llm,
            tavily,
            llm_server,
            tavily_server,
        };
        harness
            .configure_model_and_web(&format!("http://{llm_address}/v1"))
            .await;
        harness
    }

    async fn configure_model_and_web(&self, base_url: &str) {
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
                    json!({
                        "model": {
                            "provider": "lmstudio",
                            "model": "test-model",
                            "baseUrl": base_url
                        },
                        "toolsets": {"web": true}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(updated.status(), StatusCode::OK);
        let updated = json_body(updated).await;
        assert_eq!(updated["toolsets"]["web"], true);
    }

    fn set_web_overrides(&self, search: &str, extract: &str) {
        let path = self.home.path().join("config.yaml");
        let mut document: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut web = serde_yaml_ng::Mapping::new();
        web.insert(
            serde_yaml_ng::Value::String("search_backend".to_owned()),
            serde_yaml_ng::Value::String(search.to_owned()),
        );
        web.insert(
            serde_yaml_ng::Value::String("extract_backend".to_owned()),
            serde_yaml_ng::Value::String(extract.to_owned()),
        );
        document.as_mapping_mut().unwrap().insert(
            serde_yaml_ng::Value::String("web".to_owned()),
            serde_yaml_ng::Value::Mapping(web),
        );
        std::fs::write(path, serde_yaml_ng::to_string(&document).unwrap()).unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.llm_server.abort();
        self.tavily_server.abort();
    }
}

#[derive(Default)]
struct MockLlm {
    calls: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    request_seen: Notify,
}

impl MockLlm {
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct MockTavily {
    requests: Mutex<Vec<(&'static str, Value)>>,
    slow_seen: AtomicBool,
    request_seen: Notify,
}

impl MockTavily {
    fn requests(&self) -> Vec<(&'static str, Value)> {
        self.requests.lock().unwrap().clone()
    }

    async fn wait_for_slow_request(&self) {
        timeout(WAIT_TIMEOUT, async {
            loop {
                if self.slow_seen.load(Ordering::SeqCst) {
                    return;
                }
                self.request_seen.notified().await;
            }
        })
        .await
        .expect("the slow Tavily request should start");
    }
}

async fn mock_chat_completions(
    State(llm): State<Arc<MockLlm>>,
    Json(request): Json<Value>,
) -> Response<Body> {
    let messages = request["messages"].as_array().unwrap();
    let has_tool_result = messages
        .last()
        .is_some_and(|message| message["role"] == "tool");
    let user_text = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned();
    llm.requests.lock().unwrap().push(request);
    llm.calls.fetch_add(1, Ordering::SeqCst);
    llm.request_seen.notify_waiters();

    let body = Body::from_stream(stream! {
        if !has_tool_result && user_text.contains("run web search") {
            let query = if user_text.contains("slow") {
                SLOW_SEARCH_QUERY
            } else {
                SEARCH_QUERY
            };
            let payload = tool_call_payload(
                SEARCH_CALL_ID,
                "web_search",
                json!({"query": query, "limit": 3}).to_string(),
            );
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
        } else if !has_tool_result && user_text.contains("run web extract") {
            let payload = tool_call_payload(
                EXTRACT_CALL_ID,
                "web_extract",
                json!({"urls": [EXTRACT_URL], "char_limit": 2_000}).to_string(),
            );
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
        } else {
            let text = if has_tool_result && user_text.contains("run web search") {
                "Search handled"
            } else if has_tool_result && user_text.contains("run web extract") {
                "Extract handled"
            } else {
                "Definitions checked"
            };
            let payload = json!({
                "choices": [{
                    "index": 0,
                    "delta": {"content": text},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10}
            });
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("data: {payload}\n\n")));
        }
        yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
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

async fn mock_tavily_search(
    State(tavily): State<Arc<MockTavily>>,
    Json(request): Json<Value>,
) -> Response<Body> {
    tavily
        .requests
        .lock()
        .unwrap()
        .push(("search", request.clone()));
    if request["query"] == SLOW_SEARCH_QUERY {
        tavily.slow_seen.store(true, Ordering::SeqCst);
        tavily.request_seen.notify_waiters();
        return pending::<Response<Body>>().await;
    }
    Json(json!({
        "results": [{
            "title": "Private search title",
            "url": SEARCH_URL,
            "content": RAW_BODY
        }]
    }))
    .into_response()
}

async fn mock_tavily_extract(
    State(tavily): State<Arc<MockTavily>>,
    Json(request): Json<Value>,
) -> Response<Body> {
    tavily.requests.lock().unwrap().push(("extract", request));
    Json(json!({
        "results": [{
            "url": EXTRACT_URL,
            "title": "Private extract title",
            "raw_content": RAW_BODY
        }],
        "failed_results": [],
        "failed_urls": []
    }))
    .into_response()
}

#[tokio::test]
async fn web_definitions_are_strict_and_injected_per_capability_readiness() {
    let search_harness = Harness::new().await;
    search_harness.set_web_overrides("tavily", "not-implemented");
    run_to_completion(
        &search_harness.app,
        "web-search-definitions-session",
        "web-search-definitions-run",
        "inspect web definitions",
    )
    .await;
    let request = search_harness.llm.requests().remove(0);
    let definitions = request["tools"].as_array().unwrap();
    assert_eq!(tool_names(definitions), ["web_search"]);
    assert_no_browser_definitions(definitions);
    let search = &definitions[0]["function"]["parameters"];
    assert_eq!(search["type"], "object");
    assert_eq!(search["additionalProperties"], false);
    assert_eq!(search["required"], json!(["query"]));
    assert_eq!(search["properties"]["query"]["maxLength"], 4_000);
    assert_eq!(search["properties"]["limit"]["minimum"], 1);
    assert_eq!(search["properties"]["limit"]["maximum"], 100);

    let extract_harness = Harness::new().await;
    extract_harness.set_web_overrides("not-implemented", "tavily");
    run_to_completion(
        &extract_harness.app,
        "web-extract-definitions-session",
        "web-extract-definitions-run",
        "inspect web definitions",
    )
    .await;
    let request = extract_harness.llm.requests().remove(0);
    let definitions = request["tools"].as_array().unwrap();
    assert_eq!(tool_names(definitions), ["web_extract"]);
    assert_no_browser_definitions(definitions);
    let extract = &definitions[0]["function"]["parameters"];
    assert_eq!(extract["type"], "object");
    assert_eq!(extract["additionalProperties"], false);
    assert_eq!(extract["required"], json!(["urls"]));
    assert_eq!(extract["properties"]["urls"]["maxItems"], 5);
    assert_eq!(extract["properties"]["char_limit"]["minimum"], 2_000);
    assert_eq!(extract["properties"]["char_limit"]["maximum"], 500_000);
}

#[tokio::test]
async fn enabled_web_toolset_injects_no_web_or_browser_tools_after_key_deletion() {
    let harness = Harness::new().await;
    let deleted = authorized_request(
        &harness.app,
        Method::DELETE,
        "/api/v1/profiles/default/secrets/TAVILY_API_KEY",
        Body::empty(),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let config = get_json(&harness.app, "/api/v1/profiles/default/config").await;
    assert_eq!(config["toolsets"]["web"], true);
    run_to_completion(
        &harness.app,
        "web-missing-key-session",
        "web-missing-key-run",
        "inspect web definitions",
    )
    .await;
    let requests = harness.llm.requests();
    assert_eq!(requests.len(), 1);
    let definitions = requests[0]["tools"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let names = tool_names(definitions);
    assert!(!names.contains(&"web_search"));
    assert!(!names.contains(&"web_extract"));
    assert_no_browser_definitions(definitions);
}

#[tokio::test]
async fn web_search_executes_privately_and_public_journal_replays_without_arguments() {
    let harness = Harness::new().await;
    let request = run_request("web-search-request", "run web search now");
    let (session_id, run_id, events, run, messages) = run_case(
        &harness.app,
        "web-search-session",
        "web-search-run",
        &request,
    )
    .await;

    assert_completed_web_tool(&events, "web_search", "Web search requested");
    assert_eq!(messages["items"][1]["parts"][0]["text"], "Search handled");
    let tavily = harness.tavily.requests();
    assert_eq!(tavily.len(), 1);
    assert_eq!(tavily[0].0, "search");
    assert_eq!(tavily[0].1["api_key"], TAVILY_KEY);
    assert_eq!(tavily[0].1["query"], SEARCH_QUERY);
    assert_eq!(tavily[0].1["max_results"], 3);
    assert_eq!(tavily[0].1["include_raw_content"], false);
    assert_eq!(tavily[0].1["include_images"], false);

    let provider_requests = harness.llm.requests();
    assert_eq!(provider_requests.len(), 2);
    let private_tool_result = provider_tool_content(&provider_requests[1], SEARCH_CALL_ID);
    assert!(private_tool_result.contains(SEARCH_URL));
    assert!(private_tool_result.contains(RAW_BODY));
    assert!(!private_tool_result.contains(TAVILY_KEY));
    assert_eq!(
        serde_json::from_str::<Value>(private_tool_result).unwrap()["externalUntrusted"],
        true
    );
    assert_public_redacted(
        &events,
        &run,
        &messages,
        &[SEARCH_QUERY, SEARCH_URL, RAW_BODY],
    );

    let cursor = events[3].id.clone();
    let replayed = collect_events(events_request(&harness.app, &run_id, Some(&cursor)).await).await;
    assert_eq!(replayed, events[4..]);
    let replay = post_run(&harness.app, &session_id, "web-search-run", &request).await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(json_body(replay).await["disposition"], "replayed");
    assert_eq!(harness.tavily.requests().len(), 1);
    assert_eq!(harness.llm.requests().len(), 2);
}

#[tokio::test]
async fn web_extract_executes_privately_and_public_message_contains_only_summaries() {
    let harness = Harness::new().await;
    let request = run_request("web-extract-request", "run web extract now");
    let (_session_id, _run_id, events, run, messages) = run_case(
        &harness.app,
        "web-extract-session",
        "web-extract-run",
        &request,
    )
    .await;

    assert_completed_web_tool(&events, "web_extract", "Web extraction requested");
    assert_eq!(messages["items"][1]["parts"][0]["text"], "Extract handled");
    let tavily = harness.tavily.requests();
    assert_eq!(tavily.len(), 1);
    assert_eq!(tavily[0].0, "extract");
    assert_eq!(tavily[0].1["api_key"], TAVILY_KEY);
    assert_eq!(tavily[0].1["urls"], json!([EXTRACT_URL]));
    assert_eq!(tavily[0].1["include_images"], false);

    let provider_requests = harness.llm.requests();
    assert_eq!(provider_requests.len(), 2);
    let private_tool_result = provider_tool_content(&provider_requests[1], EXTRACT_CALL_ID);
    assert!(private_tool_result.contains(EXTRACT_URL));
    assert!(private_tool_result.contains(RAW_BODY));
    assert!(!private_tool_result.contains(TAVILY_KEY));
    assert_eq!(
        serde_json::from_str::<Value>(private_tool_result).unwrap()["externalUntrusted"],
        true
    );
    assert_public_redacted(&events, &run, &messages, &[EXTRACT_URL, RAW_BODY]);
}

#[tokio::test]
async fn cancelling_an_in_flight_tavily_request_closes_the_run_without_public_leakage() {
    let harness = Harness::new().await;
    let session_id = create_session(&harness.app, "web-cancel-session").await;
    let accepted = post_run(
        &harness.app,
        &session_id,
        "web-cancel-run",
        &run_request("web-cancel-request", "run web search slow now"),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness.tavily.wait_for_slow_request().await;

    let cancelled = authorized_request(
        &harness.app,
        Method::POST,
        &format!("/api/v1/runs/{run_id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    let _ = json_body(cancelled).await;
    let run = wait_for_run_status(&harness.app, &run_id, "cancelled").await;
    let events = collect_events(events_request(&harness.app, &run_id, None).await).await;
    assert_eq!(event_names(&events).last(), Some(&"run.cancelled"));
    assert!(event_names(&events).contains(&"tool.started"));
    let messages = get_json(
        &harness.app,
        &format!("/api/v1/sessions/{session_id}/messages"),
    )
    .await;
    assert_public_redacted(&events, &run, &messages, &[SLOW_SEARCH_QUERY]);
    assert_eq!(harness.llm.requests().len(), 1);
}

fn tool_names(definitions: &[Value]) -> Vec<&str> {
    definitions
        .iter()
        .map(|definition| definition["function"]["name"].as_str().unwrap())
        .collect()
}

fn assert_no_browser_definitions(definitions: &[Value]) {
    assert!(
        tool_names(definitions)
            .into_iter()
            .all(|name| !name.starts_with("browser_"))
    );
}

fn provider_tool_content<'a>(request: &'a Value, call_id: &str) -> &'a str {
    let tool = request["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], call_id);
    tool["content"].as_str().unwrap()
}

fn assert_completed_web_tool(events: &[CapturedEvent], name: &str, input_summary: &str) {
    let started = events
        .iter()
        .find(|event| event.name == "tool.started")
        .unwrap();
    let completed = events
        .iter()
        .find(|event| event.name == "tool.completed")
        .unwrap();
    assert_eq!(started.data["data"]["name"], name);
    assert_eq!(started.data["data"]["inputSummary"], input_summary);
    let summary = completed.data["data"]["resultSummary"].as_str().unwrap();
    assert!(!summary.is_empty());
}

fn assert_public_redacted(
    events: &[CapturedEvent],
    run: &Value,
    messages: &Value,
    private_values: &[&str],
) {
    let public = format!(
        "{}{}{}",
        events
            .iter()
            .map(|event| event.data.to_string())
            .collect::<String>(),
        run,
        messages
    );
    for value in private_values.iter().copied().chain([TAVILY_KEY]) {
        assert!(!public.contains(value), "public projection leaked {value}");
    }
}

async fn run_to_completion(app: &Router, session_key: &str, run_key: &str, prompt: &str) -> Value {
    let session_id = create_session(app, session_key).await;
    let accepted = post_run(
        app,
        &session_id,
        run_key,
        &run_request(&format!("request-{run_key}"), prompt),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_for_run_status(app, &run_id, "completed").await
}

async fn run_case(
    app: &Router,
    session_key: &str,
    run_key: &str,
    request: &Value,
) -> (String, String, Vec<CapturedEvent>, Value, Value) {
    let session_id = create_session(app, session_key).await;
    let accepted = post_run(app, &session_id, run_key, request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run = wait_for_run_status(app, &run_id, "completed").await;
    let events = collect_events(events_request(app, &run_id, None).await).await;
    let messages = get_json(app, &format!("/api/v1/sessions/{session_id}/messages")).await;
    (session_id, run_id, events, run, messages)
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
                json!({"profileId": "default", "title": "Web Run HTTP test"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["id"].as_str().unwrap().to_owned()
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
    timeout(WAIT_TIMEOUT, async {
        loop {
            let run = get_json(app, &format!("/api/v1/runs/{run_id}")).await;
            if run["status"] == expected {
                return run;
            }
            assert_ne!(run["status"], "failed", "run failed unexpectedly: {run}");
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run should reach its expected status")
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

fn event_names(events: &[CapturedEvent]) -> Vec<&str> {
    events.iter().map(|event| event.name.as_str()).collect()
}
