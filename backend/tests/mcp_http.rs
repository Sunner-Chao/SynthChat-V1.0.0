use std::{fs, sync::Arc};

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router, profiles::CreateProfile};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const DEFAULT_PATH: &str = "/api/v1/profiles/default/mcp/servers";

struct Fixture {
    app: axum::Router,
    home: TempDir,
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    let app = build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        profiles,
    ));
    Fixture { app, home }
}

fn stdio_body(name: &str) -> Value {
    json!({
        "transport": "stdio",
        "name": name,
        "command": "npx",
        "args": ["-y", "@example/mcp"],
        "enabled": true,
        "timeoutSeconds": 30,
        "envSecretNames": ["MCP_TOKEN"]
    })
}

fn remote_body(name: &str) -> Value {
    json!({
        "transport": "streamableHttp",
        "name": name,
        "url": "https://example.com/mcp",
        "enabled": true,
        "timeoutSeconds": 30
    })
}

#[tokio::test]
async fn authentication_runs_before_body_parsing_and_future_routes_stay_absent() {
    let fixture = fixture();
    let unauthorized = fixture
        .app
        .clone()
        .oneshot(
            Request::post(DEFAULT_PATH)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let malformed = fixture
        .app
        .clone()
        .oneshot(
            Request::post(DEFAULT_PATH)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "malformed-key")
                .body(Body::from("not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(malformed, StatusCode::BAD_REQUEST, "invalid_json").await;

    for (method, path) in [
        (
            "POST",
            "/api/v1/profiles/default/mcp/servers/mcp_00000000000000000000000000000000/test",
        ),
        (
            "GET",
            "/api/v1/profiles/default/mcp/servers/mcp_00000000000000000000000000000000/tools",
        ),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_problem(response, StatusCode::NOT_FOUND, "route_not_found").await;
    }
}

#[tokio::test]
async fn crud_uses_strong_etags_and_durable_post_idempotency() {
    let fixture = fixture();
    let empty = authorized_get(fixture.app.clone(), DEFAULT_PATH).await;
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(empty.headers()[header::CACHE_CONTROL], "no-store");
    let empty_etag = etag(&empty);
    assert_eq!(json_body(empty).await, json!([]));

    let created = post_json(
        fixture.app.clone(),
        DEFAULT_PATH,
        "mcp-create-key1",
        stdio_body("local"),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
    let created_etag = etag(&created);
    assert_ne!(created_etag, empty_etag);
    let created_body = json_body(created).await;
    let id = created_body["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("mcp_") && id.len() == 36);
    assert_eq!(created_body["missingSecretNames"], json!(["MCP_TOKEN"]));
    assert_eq!(created_body["envSecretNames"], json!(["MCP_TOKEN"]));
    assert!(created_body["bearerTokenSecretName"].is_null());

    let replay = post_json(
        fixture.app.clone(),
        DEFAULT_PATH,
        "mcp-create-key1",
        stdio_body("local"),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(etag(&replay), created_etag);
    assert_eq!(json_body(replay).await["id"], id);

    let missing_key = fixture
        .app
        .clone()
        .oneshot(
            Request::post(DEFAULT_PATH)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(stdio_body("other").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        missing_key,
        StatusCode::BAD_REQUEST,
        "invalid_idempotency_key",
    )
    .await;

    let item_path = format!("{DEFAULT_PATH}/{id}");
    let no_precondition = patch_json(
        fixture.app.clone(),
        &item_path,
        None,
        json!({"enabled": false}),
    )
    .await;
    assert_problem(
        no_precondition,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    )
    .await;
    let weak = patch_json(
        fixture.app.clone(),
        &item_path,
        Some("W/\"weak\""),
        json!({"enabled": false}),
    )
    .await;
    assert_problem(weak, StatusCode::BAD_REQUEST, "invalid_if_match").await;

    let updated = patch_json(
        fixture.app.clone(),
        &item_path,
        Some(&created_etag),
        json!({"enabled": false, "timeoutSeconds": 31}),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(updated.headers()[header::CACHE_CONTROL], "no-store");
    let updated_etag = etag(&updated);
    assert_ne!(updated_etag, created_etag);
    let updated_body = json_body(updated).await;
    assert_eq!(updated_body["enabled"], false);
    assert_eq!(updated_body["timeoutSeconds"], 31);

    let stale = patch_json(
        fixture.app.clone(),
        &item_path,
        Some(&created_etag),
        json!({"enabled": true}),
    )
    .await;
    assert_eq!(stale.headers()[header::ETAG], updated_etag);
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    let delete_without_etag = authorized_delete(fixture.app.clone(), &item_path, None).await;
    assert_problem(
        delete_without_etag,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    )
    .await;
    let deleted = authorized_delete(fixture.app.clone(), &item_path, Some(&updated_etag)).await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(deleted.headers()[header::CACHE_CONTROL], "no-store");
    let deleted_etag = etag(&deleted);
    assert_ne!(deleted_etag, updated_etag);
    let delete_replay =
        authorized_delete(fixture.app.clone(), &item_path, Some(&deleted_etag)).await;
    assert_eq!(delete_replay.status(), StatusCode::NO_CONTENT);
    assert_eq!(etag(&delete_replay), deleted_etag);
    assert_eq!(
        json_body(authorized_get(fixture.app, DEFAULT_PATH).await).await,
        json!([])
    );
}

#[tokio::test]
async fn semantic_validation_and_stored_secret_errors_are_static_and_redacted() {
    let fixture = fixture();
    let sensitive = post_json(
        fixture.app.clone(),
        DEFAULT_PATH,
        "sensitive-key01",
        json!({
            "transport": "stdio",
            "name": "bad",
            "command": "npx",
            "args": ["--api-key=super-secret-value"],
            "enabled": true,
            "timeoutSeconds": 30,
            "envSecretNames": []
        }),
    )
    .await;
    let sensitive = problem_body(sensitive, StatusCode::BAD_REQUEST, "validation_failed").await;
    let serialized = sensitive.to_string();
    assert!(!serialized.contains("super-secret-value"));
    assert!(!serialized.contains(fixture.home.path().to_string_lossy().as_ref()));

    fs::write(
        fixture.home.path().join("config.yaml"),
        "mcp_servers:\n  leaked:\n    url: https://example.com/mcp\n    headers:\n      Authorization: Bearer literal-secret-value\n",
    )
    .unwrap();
    let stored = authorized_get(fixture.app, DEFAULT_PATH).await;
    let stored = problem_body(stored, StatusCode::CONFLICT, "mcp_config_invalid").await;
    let serialized = stored.to_string();
    assert!(!serialized.contains("literal-secret-value"));
    assert!(!serialized.contains(fixture.home.path().to_string_lossy().as_ref()));
    assert!(!serialized.contains("config.yaml"));
}

#[tokio::test]
async fn transport_fields_are_strict_and_transport_switching_is_rejected() {
    let fixture = fixture();
    let unknown = post_json(
        fixture.app.clone(),
        DEFAULT_PATH,
        "unknown-key-001",
        json!({
            "transport": "stdio",
            "name": "bad",
            "command": "npx",
            "args": [],
            "enabled": true,
            "timeoutSeconds": 30,
            "envSecretNames": [],
            "url": "https://example.com/mcp"
        }),
    )
    .await;
    assert_problem(unknown, StatusCode::BAD_REQUEST, "invalid_json").await;
    let query = post_json(
        fixture.app.clone(),
        DEFAULT_PATH,
        "query-url-key1",
        json!({
            "transport": "streamableHttp",
            "name": "bad-url",
            "url": "https://example.com/mcp?token=secret",
            "enabled": true,
            "timeoutSeconds": 30
        }),
    )
    .await;
    assert_problem(query, StatusCode::BAD_REQUEST, "validation_failed").await;

    let created = post_json(
        fixture.app.clone(),
        DEFAULT_PATH,
        "remote-key-0001",
        remote_body("remote"),
    )
    .await;
    let created_etag = etag(&created);
    let id = json_body(created).await["id"].as_str().unwrap().to_owned();
    let switched = patch_json(
        fixture.app,
        &format!("{DEFAULT_PATH}/{id}"),
        Some(&created_etag),
        json!({"transport": "sse"}),
    )
    .await;
    assert_problem(switched, StatusCode::BAD_REQUEST, "validation_failed").await;
}

#[tokio::test]
async fn profile_scopes_allow_the_same_name_without_aliasing_ids_or_storage() {
    let home = tempfile::tempdir().unwrap();
    let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    profiles
        .create_profile(
            &CreateProfile {
                id: "work".to_owned(),
                display_name: "Work".to_owned(),
                clone_from_profile_id: None,
            },
            "create-work-profile",
        )
        .unwrap();
    let app = build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        profiles,
    ));
    let default = post_json(
        app.clone(),
        DEFAULT_PATH,
        "default-mcp-key",
        remote_body("shared"),
    )
    .await;
    let default_id = json_body(default).await["id"].as_str().unwrap().to_owned();
    let work_path = "/api/v1/profiles/work/mcp/servers";
    let work = post_json(
        app.clone(),
        work_path,
        "work-mcp-key-001",
        remote_body("shared"),
    )
    .await;
    let work_id = json_body(work).await["id"].as_str().unwrap().to_owned();
    assert_ne!(default_id, work_id);
    assert_eq!(
        json_body(authorized_get(app.clone(), DEFAULT_PATH).await).await[0]["id"],
        default_id
    );
    assert_eq!(
        json_body(authorized_get(app, work_path).await).await[0]["id"],
        work_id
    );
    assert!(home.path().join("config.yaml").is_file());
    assert!(home.path().join("profiles/work/config.yaml").is_file());
}

#[tokio::test]
async fn capabilities_report_the_verified_stdio_runtime_slice() {
    let fixture = fixture();
    let capabilities = json_body(authorized_get(fixture.app, "/api/v1/capabilities").await).await;
    assert_eq!(capabilities["engine"]["features"]["mcpManagement"], true);
    assert_eq!(capabilities["extensions"]["mcpStdio"], true);
    assert_eq!(capabilities["extensions"]["mcpStreamableHttp"], true);
    assert_eq!(capabilities["extensions"]["mcpSse"], true);
}

#[tokio::test]
async fn keychain_unavailable_is_503_before_a_secret_bearing_mutation() {
    let home = tempfile::tempdir().unwrap();
    let profiles = ProfileService::without_credential_store(home.path().to_owned());
    let app = build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        profiles,
    ));
    let public = post_json(
        app.clone(),
        DEFAULT_PATH,
        "public-mcp-key1",
        remote_body("public"),
    )
    .await;
    assert_eq!(public.status(), StatusCode::CREATED);
    let before = fs::read(home.path().join("config.yaml")).unwrap();
    let blocked = post_json(
        app.clone(),
        DEFAULT_PATH,
        "blocked-mcp-key",
        stdio_body("blocked"),
    )
    .await;
    assert_problem(
        blocked,
        StatusCode::SERVICE_UNAVAILABLE,
        "secret_storage_unavailable",
    )
    .await;
    assert_eq!(fs::read(home.path().join("config.yaml")).unwrap(), before);
    assert_eq!(
        json_body(authorized_get(app, DEFAULT_PATH).await)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn post_json(app: axum::Router, path: &str, key: &str, body: Value) -> Response<Body> {
    app.oneshot(
        Request::post(path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn patch_json(
    app: axum::Router,
    path: &str,
    etag: Option<&str>,
    body: Value,
) -> Response<Body> {
    let mut request = Request::patch(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/merge-patch+json");
    if let Some(etag) = etag {
        request = request.header(header::IF_MATCH, etag);
    }
    app.oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn authorized_get(app: axum::Router, path: &str) -> Response<Body> {
    app.oneshot(
        Request::get(path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn authorized_delete(app: axum::Router, path: &str, etag: Option<&str>) -> Response<Body> {
    let mut request =
        Request::delete(path).header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if let Some(etag) = etag {
        request = request.header(header::IF_MATCH, etag);
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn etag(response: &Response<Body>) -> String {
    let value = response.headers()[header::ETAG].to_str().unwrap();
    assert!(value.starts_with('"') && value.ends_with('"') && !value.starts_with("W/"));
    value.to_owned()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    let _ = problem_body(response, status, code).await;
}

async fn problem_body(response: Response<Body>, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert!(response.headers().contains_key("x-request-id"));
    let body = json_body(response).await;
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code);
    body
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
