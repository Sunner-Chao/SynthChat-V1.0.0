use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode, header},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const QR_CODE: &str = "fixture-qr-ticket";
const SECOND_QR_CODE: &str = "fixture-qr-ticket-2";
const BOT_ID: &str = "fixture-bot-id";
const SECOND_BOT_ID: &str = "fixture-bot-id-2";
const BOT_TOKEN: &str = "FIXTURE_WECHAT_TOKEN_MUST_NOT_LEAK";
const SECOND_BOT_TOKEN: &str = "FIXTURE_WECHAT_TOKEN_2_MUST_NOT_LEAK";

#[derive(Clone, Debug, PartialEq)]
struct UpstreamRequest {
    app_id: String,
    client_version: String,
    authorization: String,
    authorization_type: String,
    query: HashMap<String, String>,
    payload: Option<Value>,
}

struct MockWechat {
    qr_start_requests: Mutex<Vec<UpstreamRequest>>,
    qr_status_requests: Mutex<Vec<UpstreamRequest>>,
    poll_requests: Mutex<Vec<UpstreamRequest>>,
    send_requests: Mutex<Vec<UpstreamRequest>>,
    poll_response: Mutex<Value>,
    send_response: Mutex<Value>,
}

impl Default for MockWechat {
    fn default() -> Self {
        Self {
            qr_start_requests: Mutex::new(Vec::new()),
            qr_status_requests: Mutex::new(Vec::new()),
            poll_requests: Mutex::new(Vec::new()),
            send_requests: Mutex::new(Vec::new()),
            poll_response: Mutex::new(json!({
                "errcode": 0,
                "get_updates_buf": "fixture-next-cursor",
                "msgs": [
                    {
                        "message_id": "incoming-message-1",
                        "from_user_id": "peer-1",
                        "item_list": [{"type": 1, "text_item": {"text": " Hello\r\nworld "}}]
                    },
                    {"message_id": "skip-me", "from_user_id": "peer-2", "item_list": []}
                ]
            })),
            send_response: Mutex::new(json!({
                "errcode": 0,
                "data": {
                    "message_id": "outgoing-message-1",
                    "bot_token": BOT_TOKEN
                }
            })),
        }
    }
}

struct Harness {
    app: Router,
    profiles: ProfileService,
    home: TempDir,
    upstream: Arc<MockWechat>,
    upstream_server: JoinHandle<()>,
    base_url: String,
}

impl Harness {
    async fn new() -> Self {
        let upstream = Arc::new(MockWechat::default());
        let upstream_app = Router::new()
            .route("/ilink/bot/get_bot_qrcode", get(mock_qr_start))
            .route("/ilink/bot/get_qrcode_status", get(mock_qr_status))
            .route("/ilink/bot/getupdates", post(mock_get_updates))
            .route("/ilink/bot/sendmessage", post(mock_send_message))
            .with_state(upstream.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_server = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });

        let home = tempfile::tempdir().unwrap();
        let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
        let app = build_router(AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles.clone(),
        ));
        Self {
            app,
            profiles,
            home,
            upstream,
            upstream_server,
            base_url: format!("http://{address}"),
        }
    }

    async fn get_wechat_config(&self) -> (String, Value) {
        let response = authorized_request(
            &self.app,
            Request::get("/api/v1/profiles/default/wechat")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let etag = response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        (etag, json_body(response).await)
    }

    async fn configure_loopback_upstream(&self, timeout_seconds: u64) -> Value {
        let (etag, _) = self.get_wechat_config().await;
        let response = authorized_request(
            &self.app,
            Request::patch("/api/v1/profiles/default/wechat")
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, etag)
                .body(Body::from(
                    json!({
                        "baseUrl": self.base_url,
                        "timeoutSeconds": timeout_seconds
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    }

    async fn confirm_account(&self, qrcode: &str) -> Value {
        let response = authorized_request(
            &self.app,
            Request::post("/api/v1/profiles/default/wechat/qr/status")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "qrcode": qrcode }).to_string()))
                .unwrap(),
        )
        .await;
        if response.status() != StatusCode::OK {
            let status = response.status();
            let problem = json_body(response).await;
            panic!("WeChat QR confirmation failed with {status}: {problem}");
        }
        json_body(response).await
    }

    async fn create_persona(&self, name: &str) -> String {
        let response = authorized_request(
            &self.app,
            Request::post("/api/v1/profiles/default/personas")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await["id"].as_str().unwrap().to_owned()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.upstream_server.abort();
    }
}

#[tokio::test]
async fn config_is_versioned_persisted_and_capability_gated() {
    let harness = Harness::new().await;
    let unauthorized = harness
        .app
        .clone()
        .oneshot(
            Request::get("/api/v1/profiles/default/wechat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let capabilities = authorized_request(
        &harness.app,
        Request::get("/api/v1/capabilities")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let capabilities = json_body(capabilities).await;
    assert_eq!(capabilities["extensions"]["wechatAccounts"], true);
    assert_eq!(capabilities["extensions"]["wechatMessaging"], true);

    let (_, initial) = harness.get_wechat_config().await;
    assert_eq!(initial["baseUrl"], "https://ilinkai.weixin.qq.com");
    assert_eq!(initial["timeoutSeconds"], 35);
    assert_eq!(initial["accounts"], json!([]));

    let updated = harness.configure_loopback_upstream(12).await;
    assert_eq!(updated["baseUrl"], harness.base_url);
    assert_eq!(updated["timeoutSeconds"], 12);
    assert_eq!(updated["accounts"], json!([]));

    let profile = harness.profiles.get_config("default").unwrap();
    assert_eq!(
        profile.value.extensions["wechat"]["baseUrl"],
        harness.base_url
    );
    assert_eq!(profile.value.extensions["wechat"]["timeoutSeconds"], 12);
    assert!(!profile.value.extensions.contains_key("EXTENSION_KEY"));
}

#[tokio::test]
async fn qr_start_uses_the_ilink_contract_and_returns_a_local_svg() {
    let harness = Harness::new().await;
    harness.configure_loopback_upstream(10).await;

    let response = authorized_request(
        &harness.app,
        Request::post("/api/v1/profiles/default/wechat/qr")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = json_body(response).await;
    assert_eq!(body["qrcode"], QR_CODE);
    assert_eq!(body["baseUrl"], harness.base_url);
    assert!(
        body["qrImage"]
            .as_str()
            .is_some_and(|value| value.starts_with("data:image/svg+xml;base64,"))
    );

    let requests = harness.upstream.qr_start_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].app_id, "bot");
    assert_eq!(requests[0].client_version, "132097");
    assert_eq!(
        requests[0].query.get("bot_type").map(String::as_str),
        Some("3")
    );
}

#[tokio::test]
async fn confirmed_qr_status_persists_only_metadata_and_a_keychain_secret() {
    let harness = Harness::new().await;
    harness.configure_loopback_upstream(10).await;

    let body = harness.confirm_account(QR_CODE).await;
    assert_eq!(body["status"], "confirmed");
    assert_eq!(body["message"], "login complete");
    assert_eq!(body["host"], "ilinkai.weixin.qq.com");
    assert_eq!(body["account"]["id"], BOT_ID);
    assert_eq!(body["account"]["note"], "Fixture account");
    assert_eq!(body["account"]["ilinkUserId"], "fixture-user-id");
    assert_eq!(body["account"]["linkedPersonaId"], Value::Null);
    assert_eq!(body["account"]["credentialConfigured"], true);
    assert!(!body.to_string().contains(BOT_TOKEN));

    {
        let requests = harness.upstream.qr_status_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].app_id, "bot");
        assert_eq!(requests[0].client_version, "132097");
        assert_eq!(
            requests[0].query.get("qrcode").map(String::as_str),
            Some(QR_CODE)
        );
    }

    let (_, config) = harness.get_wechat_config().await;
    assert_eq!(config["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(config["accounts"][0]["id"], BOT_ID);
    assert_eq!(config["accounts"][0]["credentialConfigured"], true);
    assert!(!config.to_string().contains(BOT_TOKEN));

    let wechat_secrets = harness
        .profiles
        .list_secret_statuses("default")
        .unwrap()
        .into_iter()
        .filter(|status| status.name.starts_with("WECHAT_BOT_"))
        .collect::<Vec<_>>();
    assert_eq!(wechat_secrets.len(), 1);
    assert!(wechat_secrets[0].configured);
    assert!(!wechat_secrets[0].name.contains(BOT_ID));

    let persisted = std::fs::read_to_string(harness.home.path().join("config.yaml")).unwrap();
    assert!(persisted.contains(BOT_ID));
    assert!(!persisted.contains(BOT_TOKEN));
}

#[tokio::test]
async fn account_link_is_profile_scoped_unique_and_etag_protected() {
    let harness = Harness::new().await;
    harness.configure_loopback_upstream(10).await;
    harness.confirm_account(QR_CODE).await;
    let persona_id = harness.create_persona("Fixture persona").await;

    let (etag, _) = harness.get_wechat_config().await;
    let linked = authorized_request(
        &harness.app,
        Request::patch(format!("/api/v1/profiles/default/wechat/accounts/{BOT_ID}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, etag)
            .body(Body::from(
                json!({ "linkedPersonaId": persona_id }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(linked.status(), StatusCode::OK);
    let linked = json_body(linked).await;
    assert_eq!(linked["accounts"][0]["linkedPersonaId"], persona_id);

    harness.confirm_account(SECOND_QR_CODE).await;
    let (etag, _) = harness.get_wechat_config().await;
    let conflict = authorized_request(
        &harness.app,
        Request::patch(format!(
            "/api/v1/profiles/default/wechat/accounts/{SECOND_BOT_ID}"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::IF_MATCH, etag)
        .body(Body::from(
            json!({ "linkedPersonaId": persona_id }).to_string(),
        ))
        .unwrap(),
    )
    .await;
    assert_problem(
        conflict,
        StatusCode::CONFLICT,
        "wechat_persona_link_conflict",
    )
    .await;

    let (etag, _) = harness.get_wechat_config().await;
    let unlinked = authorized_request(
        &harness.app,
        Request::patch(format!("/api/v1/profiles/default/wechat/accounts/{BOT_ID}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, etag)
            .body(Body::from(json!({ "linkedPersonaId": null }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(unlinked.status(), StatusCode::OK);
    assert_eq!(
        json_body(unlinked).await["accounts"][0]["linkedPersonaId"],
        Value::Null
    );
}

#[tokio::test]
async fn poll_and_send_use_keychain_token_and_never_expose_upstream_payloads() {
    let harness = Harness::new().await;
    harness.configure_loopback_upstream(10).await;
    harness.confirm_account(QR_CODE).await;

    let polled = authorized_request(
        &harness.app,
        Request::post(format!(
            "/api/v1/profiles/default/wechat/accounts/{BOT_ID}/poll"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "cursor": "fixture-cursor" }).to_string(),
        ))
        .unwrap(),
    )
    .await;
    assert_eq!(polled.status(), StatusCode::OK);
    let polled = json_body(polled).await;
    assert_eq!(polled["receivedCount"], 2);
    assert_eq!(polled["skippedCount"], 1);
    assert_eq!(polled["nextCursor"], "fixture-next-cursor");
    assert_eq!(polled["messages"][0]["id"], "incoming-message-1");
    assert_eq!(polled["messages"][0]["peer"], "peer-1");
    assert_eq!(polled["messages"][0]["text"], "Hello\nworld");
    assert!(!polled.to_string().contains(BOT_TOKEN));

    {
        let poll_requests = harness.upstream.poll_requests.lock().unwrap();
        assert_eq!(poll_requests.len(), 1);
        assert_eq!(
            poll_requests[0].authorization,
            format!("Bearer {BOT_TOKEN}")
        );
        assert_eq!(poll_requests[0].authorization_type, "ilink_bot_token");
        assert_eq!(
            poll_requests[0].payload.as_ref().unwrap()["get_updates_buf"],
            "fixture-cursor"
        );
        assert_eq!(
            poll_requests[0].payload.as_ref().unwrap()["base_info"]["channel_version"],
            "2.4.6"
        );
    }

    let sent = authorized_request(
        &harness.app,
        Request::post(format!(
            "/api/v1/profiles/default/wechat/accounts/{BOT_ID}/messages"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "peer": "peer-1", "text": " Hello\r\nworld " }).to_string(),
        ))
        .unwrap(),
    )
    .await;
    assert_eq!(sent.status(), StatusCode::OK);
    let sent = json_body(sent).await;
    assert_eq!(sent["accepted"], true);
    assert_eq!(sent["messageId"], "outgoing-message-1");
    assert!(!sent.to_string().contains(BOT_TOKEN));

    {
        let send_requests = harness.upstream.send_requests.lock().unwrap();
        assert_eq!(send_requests.len(), 1);
        assert_eq!(
            send_requests[0].authorization,
            format!("Bearer {BOT_TOKEN}")
        );
        assert_eq!(send_requests[0].authorization_type, "ilink_bot_token");
        let message = &send_requests[0].payload.as_ref().unwrap()["msg"];
        assert_eq!(message["to_user_id"], "peer-1");
        assert_eq!(message["message_type"], 2);
        assert_eq!(message["message_state"], 2);
        assert_eq!(message["item_list"][0]["text_item"]["text"], "Hello\nworld");
    }

    let secret_name = harness
        .profiles
        .list_secret_statuses("default")
        .unwrap()
        .into_iter()
        .find(|status| status.name.starts_with("WECHAT_BOT_"))
        .unwrap()
        .name;
    harness
        .profiles
        .delete_secret("default", &secret_name)
        .unwrap();
    let missing_credential = authorized_request(
        &harness.app,
        Request::post(format!(
            "/api/v1/profiles/default/wechat/accounts/{BOT_ID}/messages"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "peer": "peer-1", "text": "again" }).to_string(),
        ))
        .unwrap(),
    )
    .await;
    assert_problem(
        missing_credential,
        StatusCode::UNPROCESSABLE_ENTITY,
        "wechat_credential_not_configured",
    )
    .await;
    assert_eq!(harness.upstream.send_requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn poll_rejects_an_unbounded_upstream_message_batch() {
    let harness = Harness::new().await;
    harness.configure_loopback_upstream(10).await;
    harness.confirm_account(QR_CODE).await;
    *harness.upstream.poll_response.lock().unwrap() = json!({
        "errcode": 0,
        "msgs": (0..101)
            .map(|index| json!({
                "message_id": format!("message-{index}"),
                "from_user_id": "peer-1",
                "text": "hello"
            }))
            .collect::<Vec<_>>()
    });

    let response = authorized_request(
        &harness.app,
        Request::post(format!(
            "/api/v1/profiles/default/wechat/accounts/{BOT_ID}/poll"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap(),
    )
    .await;
    assert_problem(
        response,
        StatusCode::BAD_GATEWAY,
        "wechat_provider_invalid_response",
    )
    .await;
}

async fn mock_qr_start(
    State(state): State<Arc<MockWechat>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    state
        .qr_start_requests
        .lock()
        .unwrap()
        .push(capture_request(&headers, query, None));
    Json(json!({
        "data": {
            "qrcode": QR_CODE,
            "qrcode_img_content": "https://ilinkai.weixin.qq.com/confirm/fixture"
        }
    }))
}

async fn mock_qr_status(
    State(state): State<Arc<MockWechat>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let second = query.get("qrcode").map(String::as_str) == Some(SECOND_QR_CODE);
    state
        .qr_status_requests
        .lock()
        .unwrap()
        .push(capture_request(&headers, query, None));
    Json(json!({
        "data": {
            "status": "confirmed",
            "message": "login complete",
            "host": "ilinkai.weixin.qq.com",
            "bot_token": if second { SECOND_BOT_TOKEN } else { BOT_TOKEN },
            "ilink_bot_id": if second { SECOND_BOT_ID } else { BOT_ID },
            "ilink_user_id": if second { "fixture-user-id-2" } else { "fixture-user-id" },
            "nickname": if second { "Fixture account 2" } else { "Fixture account" }
        }
    }))
}

async fn mock_get_updates(
    State(state): State<Arc<MockWechat>>,
    headers: HeaderMap,
    payload: Json<Value>,
) -> Json<Value> {
    state.poll_requests.lock().unwrap().push(capture_request(
        &headers,
        HashMap::new(),
        Some(payload.0),
    ));
    Json(state.poll_response.lock().unwrap().clone())
}

async fn mock_send_message(
    State(state): State<Arc<MockWechat>>,
    headers: HeaderMap,
    payload: Json<Value>,
) -> Json<Value> {
    state.send_requests.lock().unwrap().push(capture_request(
        &headers,
        HashMap::new(),
        Some(payload.0),
    ));
    Json(state.send_response.lock().unwrap().clone())
}

fn capture_request(
    headers: &HeaderMap,
    query: HashMap<String, String>,
    payload: Option<Value>,
) -> UpstreamRequest {
    UpstreamRequest {
        app_id: header_value(headers, "iLink-App-Id"),
        client_version: header_value(headers, "iLink-App-ClientVersion"),
        authorization: header_value(headers, "Authorization"),
        authorization_type: header_value(headers, "AuthorizationType"),
        query,
        payload,
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

async fn authorized_request(app: &Router, mut request: Request<Body>) -> axum::response::Response {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    app.clone().oneshot(request).await.unwrap()
}

async fn assert_problem(response: axum::response::Response, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    let body = json_body(response).await;
    assert_eq!(body["code"], code);
    assert!(!body.to_string().contains(BOT_TOKEN));
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
