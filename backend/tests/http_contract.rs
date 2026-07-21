use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{
    AppConfig, ProfileService, SessionService, build_router,
    sessions::{CommitMessage, MessagePart, MessageRole, SESSION_SCHEMA_VERSION},
};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn app() -> (axum::Router, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let store: std::sync::Arc<keyring_core::CredentialStore> =
        keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    (
        build_router(AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        )),
        home,
    )
}

fn app_without_keychain() -> (axum::Router, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let profiles = ProfileService::without_credential_store(home.path().to_owned());
    (
        build_router(AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        )),
        home,
    )
}

fn app_with_session_service() -> (axum::Router, TempDir, SessionService) {
    let home = tempfile::tempdir().unwrap();
    let store: std::sync::Arc<keyring_core::CredentialStore> =
        keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    let sessions = SessionService::new(home.path(), TOKEN);
    let router = build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        profiles,
    ));
    (router, home, sessions)
}

fn app_with_unavailable_session_storage() -> (axum::Router, TempDir) {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".synthchat/sessions-v1.db")).unwrap();
    let store: std::sync::Arc<keyring_core::CredentialStore> =
        keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    (
        build_router(AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        )),
        home,
    )
}

#[tokio::test]
async fn health_is_public_and_matches_the_contract() {
    let (app, _home) = app();
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
    let body = json_body(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "synthchat-hermes-backend");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn capabilities_requires_the_desktop_token() {
    let (app, _home) = app();
    let response = app
        .oneshot(
            Request::get("/api/v1/capabilities")
                .header("x-request-id", "contract-test-request")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(
        response.headers()[header::WWW_AUTHENTICATE],
        "Bearer realm=\"synthchat-desktop\""
    );
    assert_eq!(response.headers()["x-request-id"], "contract-test-request");
    let body = json_body(response).await;
    assert_eq!(body["status"], 401);
    assert_eq!(body["code"], "unauthorized");
    assert_eq!(body["requestId"], "contract-test-request");
    assert_eq!(body["retryable"], false);
}

#[tokio::test]
async fn capabilities_reports_only_the_implemented_engine_features() {
    let (app, _home) = app();
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/v1/capabilities")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["contractVersion"], "v1");
    assert_eq!(body["backendVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["engine"]["kind"], "hermes-rust");
    assert_eq!(body["engine"]["available"], true);
    assert_eq!(
        body["engine"]["features"],
        serde_json::json!({
            "runStreaming": true,
            "reasoningStreaming": true,
            "toolProgress": true,
            "approvals": true,
            "clarifications": true,
            "asyncToolDelivery": true,
            "profileManagement": true,
            "skillManagement": true,
            "memoryWrite": true,
            "mcpManagement": true,
            "oauthAccounts": false
        })
    );
    assert_eq!(body["sessionStorage"]["available"], true);
    assert_eq!(
        body["sessionStorage"]["schemaVersion"],
        SESSION_SCHEMA_VERSION
    );
    assert_eq!(body["sessionStorage"]["hermesImportAvailable"], true);
    assert_eq!(body["sessionSearch"]["mode"], "fts5");
    assert_eq!(body["files"]["maxBytes"], 8 * 1024 * 1024);
    assert_eq!(
        body["files"]["allowedMimeTypes"],
        json!([
            "application/json",
            "application/octet-stream",
            "application/pdf",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/x-zip-compressed",
            "application/yaml",
            "application/zip",
            "image/gif",
            "image/jpeg",
            "image/png",
            "image/webp",
            "text/csv",
            "text/markdown",
            "text/plain",
            "text/tab-separated-values",
            "text/yaml"
        ])
    );
    let code_execution = body["extensions"]["codeExecution"]
        .as_bool()
        .expect("codeExecution must be a stable boolean capability");
    let browser_automation = body["extensions"]["browserAutomation"]
        .as_bool()
        .expect("browserAutomation must be a stable boolean capability");
    let browser_cdp = body["extensions"]["browserCdp"]
        .as_bool()
        .expect("browserCdp must be a stable boolean capability");
    let browser_downloads = body["extensions"]["browserDownloads"]
        .as_bool()
        .expect("browserDownloads must be a stable boolean capability");
    assert_eq!(browser_automation, browser_cdp);
    assert_eq!(browser_automation, browser_downloads);
    assert_eq!(
        body["extensions"],
        serde_json::json!({
            "activeRunDiscovery": true,
            "runQueue": true,
            "codeExecution": code_execution,
            "skillDiscovery": true,
            "skillEnablement": true,
            "toolExecution": true,
            "toolsetManagement": true,
            "workspaceManagement": true,
            "webSearch": true,
            "webExtract": true,
            "browserAutomation": browser_automation,
            "browserCdp": browser_cdp,
            "browserDownloads": browser_downloads,
            "mcpStdio": true,
            "mcpStreamableHttp": true,
            "mcpSse": true,
            "wechatAccounts": true,
            "wechatMessaging": true,
            "plugins": true,
            "personas": true,
            "moments": true,
            "worldbooks": true
        })
    );

    let toolsets = json_body(authorized_get(app, "/api/v1/profiles/default/toolsets").await).await;
    assert_eq!(
        toolsets
            .as_array()
            .unwrap()
            .iter()
            .find(|toolset| toolset["id"] == "code_execution")
            .unwrap()["configured"],
        code_execution
    );
}

#[tokio::test]
async fn unavailable_session_storage_is_reported_and_session_routes_return_503() {
    let (app, _home) = app_with_unavailable_session_storage();
    let capabilities = authorized_get(app.clone(), "/api/v1/capabilities").await;
    assert_eq!(capabilities.status(), StatusCode::OK);
    let capabilities = json_body(capabilities).await;
    assert_eq!(capabilities["sessionStorage"]["available"], false);
    assert_eq!(capabilities["sessionStorage"]["schemaVersion"], Value::Null);
    assert_eq!(
        capabilities["sessionStorage"]["hermesImportAvailable"],
        false
    );
    assert_eq!(capabilities["sessionSearch"]["mode"], "unavailable");
    assert_eq!(capabilities["extensions"]["codeExecution"], false);
    assert_eq!(
        capabilities["engine"]["features"]["asyncToolDelivery"],
        false
    );

    let profiles = authorized_get(app.clone(), "/api/v1/profiles").await;
    assert_eq!(profiles.status(), StatusCode::OK);
    let profiles = json_body(profiles).await;
    assert_eq!(profiles[0]["engineState"], "failed");
    let list = authorized_get(app.clone(), "/api/v1/sessions?profileId=default").await;
    assert_problem(
        list,
        StatusCode::SERVICE_UNAVAILABLE,
        "session_storage_unavailable",
    )
    .await;

    let create = app
        .oneshot(
            Request::post("/api/v1/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "unavailable-session-storage")
                .body(Body::from(
                    r#"{"profileId":"default","title":"Unavailable"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        create,
        StatusCode::SERVICE_UNAVAILABLE,
        "session_storage_unavailable",
    )
    .await;
}

#[tokio::test]
async fn cors_allows_only_configured_origins() {
    let (app, _home) = app();
    let allowed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/capabilities")
                .header(header::ORIGIN, "tauri://localhost")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "tauri://localhost"
    );

    let denied = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/capabilities")
                .header(header::ORIGIN, "https://example.com")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !denied
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
}

#[tokio::test]
async fn profile_crud_config_and_secret_flow_matches_the_contract() {
    let (app, home) = app();
    let create = app
        .clone()
        .oneshot(
            Request::post("/api/v1/profiles")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "create-work-profile")
                .body(Body::from(
                    r#"{"id":"work","displayName":"Work","cloneFromProfileId":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let metadata_etag = create.headers()[header::ETAG].to_str().unwrap().to_owned();
    let created = json_body(create).await;
    assert_eq!(created["id"], "work");
    assert_eq!(created["displayName"], "Work");
    assert!(created.get("isActive").is_none());
    assert!(created.get("configRevision").is_none());

    let replay = app
        .clone()
        .oneshot(
            Request::post("/api/v1/profiles")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "create-work-profile")
                .body(Body::from(
                    r#"{"id":"work","displayName":"Work","cloneFromProfileId":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.headers()[header::ETAG], metadata_etag);

    let missing_precondition = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/work")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .body(Body::from(r##"{"color":"#336699"}"##))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        missing_precondition,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    )
    .await;

    let malformed_precondition = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/work")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, format!("W/{metadata_etag}"))
                .body(Body::from(r##"{"color":"#336699"}"##))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        malformed_precondition,
        StatusCode::BAD_REQUEST,
        "invalid_if_match",
    )
    .await;

    let updated_metadata = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/work")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &metadata_etag)
                .body(Body::from(r##"{"color":"#336699"}"##))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated_metadata.status(), StatusCode::OK);
    let new_metadata_etag = updated_metadata.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(metadata_etag, new_metadata_etag);
    assert_eq!(json_body(updated_metadata).await["color"], "#336699");

    let config = authorized_get(app.clone(), "/api/v1/profiles/work/config").await;
    assert_eq!(config.status(), StatusCode::OK);
    let config_etag = config.headers()[header::ETAG].to_str().unwrap().to_owned();
    let config_body = json_body(config).await;
    assert_eq!(config_body["revision"], config_etag.trim_matches('"'));
    assert_eq!(
        config_body["codeExecution"],
        serde_json::json!({
            "mode": "project",
            "timeoutSeconds": 300,
            "maxToolCalls": 50
        })
    );

    let updated_config = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/work/config")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &config_etag)
                .body(Body::from(
                    r#"{"model":{"provider":"openrouter","model":"provider/model","baseUrl":null},"codeExecution":{"mode":"strict","timeoutSeconds":120,"maxToolCalls":7}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated_config.status(), StatusCode::OK);
    let new_config_etag = updated_config.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(config_etag, new_config_etag);
    let updated_config = json_body(updated_config).await;
    assert_eq!(
        updated_config["codeExecution"],
        serde_json::json!({
            "mode": "strict",
            "timeoutSeconds": 120,
            "maxToolCalls": 7
        })
    );
    let persisted = std::fs::read_to_string(
        home.path()
            .join("profiles")
            .join("work")
            .join("config.yaml"),
    )
    .unwrap();
    assert!(persisted.contains("mode: strict"));
    assert!(persisted.contains("timeout: 120"));
    assert!(persisted.contains("max_tool_calls: 7"));

    let invalid_config = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/work/config")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &new_config_etag)
                .body(Body::from(r#"{"codeExecution":{"timeoutSeconds":0}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        invalid_config,
        StatusCode::BAD_REQUEST,
        "invalid_profile_config",
    )
    .await;

    let conflict = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/work/config")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &config_etag)
                .body(Body::from(r#"{"model":{"model":"stale"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conflict.headers()[header::ETAG], new_config_etag);
    assert_problem(conflict, StatusCode::CONFLICT, "revision_conflict").await;

    let secret_value = "secret-that-must-never-be-returned";
    let put_secret = app
        .clone()
        .oneshot(
            Request::put("/api/v1/profiles/work/secrets/OPENROUTER_API_KEY")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"value":"{secret_value}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put_secret.status(), StatusCode::OK);
    let put_body = serde_json::to_string(&json_body(put_secret).await).unwrap();
    assert!(!put_body.contains(secret_value));
    assert!(!put_body.contains("preview"));

    let statuses = authorized_get(app.clone(), "/api/v1/profiles/work/secrets").await;
    let statuses = json_body(statuses).await;
    assert!(
        statuses.as_array().unwrap().iter().any(|status| {
            status["name"] == "OPENROUTER_API_KEY" && status["configured"] == true
        })
    );

    let activate = app
        .clone()
        .oneshot(
            Request::put("/api/v1/profiles/work/active")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(activate.status(), StatusCode::OK);
    let activated = json_body(activate).await;
    assert_eq!(activated["isActive"], true);
    assert_eq!(activated["engineState"], "running");

    let profiles = json_body(authorized_get(app.clone(), "/api/v1/profiles").await).await;
    assert!(
        profiles
            .as_array()
            .unwrap()
            .iter()
            .all(|profile| profile["engineState"] == "running")
    );

    let active_delete = authorized_delete(app.clone(), "/api/v1/profiles/work").await;
    assert_problem(
        active_delete,
        StatusCode::CONFLICT,
        "profile_delete_conflict",
    )
    .await;
    let activate_default = app
        .clone()
        .oneshot(
            Request::put("/api/v1/profiles/default/active")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(activate_default.status(), StatusCode::OK);
    assert_eq!(
        authorized_delete(app.clone(), "/api/v1/profiles/work")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        authorized_delete(app, "/api/v1/profiles/work")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn profile_routes_return_problem_details_for_transport_errors() {
    let (app, _home) = app();
    let unauthorized_unknown = app
        .clone()
        .oneshot(
            Request::get("/api/v1/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        unauthorized_unknown,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    )
    .await;

    let unknown = authorized_get(app.clone(), "/api/v1/does-not-exist").await;
    assert_problem(unknown, StatusCode::NOT_FOUND, "route_not_found").await;

    let method = app
        .clone()
        .oneshot(
            Request::post("/api/v1/capabilities")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(method, StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed").await;

    let invalid_json = app
        .oneshot(
            Request::post("/api/v1/profiles")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "invalid-json-key")
                .body(Body::from("{not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(invalid_json, StatusCode::BAD_REQUEST, "invalid_json").await;
}

#[tokio::test]
async fn oversized_profile_json_returns_payload_too_large_problem() {
    let (app, _home) = app();
    let response = app
        .oneshot(
            Request::post("/api/v1/profiles")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "oversized-profile-body")
                .body(Body::from(vec![b'x'; 1024 * 1024 + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(response, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").await;
}

#[tokio::test]
async fn unavailable_keychain_only_degrades_secret_routes() {
    let (app, _home) = app_without_keychain();
    assert_eq!(
        authorized_get(app.clone(), "/api/v1/profiles")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        authorized_get(app.clone(), "/api/v1/profiles/default/config")
            .await
            .status(),
        StatusCode::OK
    );
    let secrets = authorized_get(app, "/api/v1/profiles/default/secrets").await;
    assert_problem(
        secrets,
        StatusCode::SERVICE_UNAVAILABLE,
        "secret_storage_unavailable",
    )
    .await;
}

#[tokio::test]
async fn profile_cors_preflight_allows_contract_headers_and_write_methods() {
    let (app, _home) = app();
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/profiles/default/config")
                .header(header::ORIGIN, "tauri://localhost")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "PATCH")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "authorization,content-type,if-match,idempotency-key",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let methods = response.headers()[header::ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .unwrap();
    assert!(methods.contains("PATCH"));
    assert!(methods.contains("DELETE"));
    let headers = response.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(headers.contains("if-match"));
    assert!(headers.contains("idempotency-key"));
}

#[tokio::test]
async fn session_crud_search_messages_and_conditions_match_the_contract() {
    let (app, _home, sessions) = app_with_session_service();
    let create_body = r#"{"profileId":"default","personaId":"persona_0123456789abcdef0123456789abcdef","title":"Contract session"}"#;
    let create = app
        .clone()
        .oneshot(
            Request::post("/api/v1/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "create-contract-session")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let initial_etag = create.headers()[header::ETAG].to_str().unwrap().to_owned();
    let location = create.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let created = json_body(create).await;
    let session_id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created["personaId"],
        "persona_0123456789abcdef0123456789abcdef"
    );
    assert_eq!(location, format!("/api/v1/sessions/{session_id}"));
    assert_eq!(
        initial_etag,
        format!("\"{}\"", created["revision"].as_str().unwrap())
    );

    let replay = app
        .clone()
        .oneshot(
            Request::post("/api/v1/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "create-contract-session")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.headers()[header::ETAG], initial_etag);

    let list = authorized_get(app.clone(), "/api/v1/sessions?profileId=default").await;
    assert_eq!(list.status(), StatusCode::OK);
    let list = json_body(list).await;
    assert_eq!(list["items"][0]["id"], session_id);
    assert_eq!(
        list["items"][0]["personaId"],
        "persona_0123456789abcdef0123456789abcdef"
    );
    assert_eq!(list["items"][0]["match"], Value::Null);

    let missing_precondition = app
        .clone()
        .oneshot(
            Request::patch(&location)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .body(Body::from(r#"{"title":"Renamed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        missing_precondition,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    )
    .await;

    let updated = app
        .clone()
        .oneshot(
            Request::patch(&location)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &initial_etag)
                .body(Body::from(r#"{"title":"Renamed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated_etag = updated.headers()[header::ETAG].to_str().unwrap().to_owned();
    assert_ne!(updated_etag, initial_etag);
    let updated = json_body(updated).await;
    assert_eq!(updated["title"], "Renamed");
    assert_eq!(
        updated["personaId"],
        "persona_0123456789abcdef0123456789abcdef"
    );

    let stale = app
        .clone()
        .oneshot(
            Request::patch(&location)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &initial_etag)
                .body(Body::from(r#"{"title":"Renamed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.headers()[header::ETAG], updated_etag);
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    sessions
        .commit_message(
            &session_id,
            &CommitMessage {
                role: MessageRole::User,
                parts: vec![MessagePart::Text {
                    text: "literal %_ NEAR search".to_owned(),
                }],
                reasoning: None,
                tool_calls: Vec::new(),
                usage: None,
                model: Some("test/model".to_owned()),
            },
        )
        .unwrap();
    let messages = authorized_get(
        app.clone(),
        &format!("/api/v1/sessions/{session_id}/messages?limit=1"),
    )
    .await;
    assert_eq!(messages.status(), StatusCode::OK);
    let messages = json_body(messages).await;
    assert_eq!(messages["snapshotLastSequence"], 1);
    assert_eq!(messages["items"][0]["sequence"], 1);
    assert_eq!(messages["items"][0]["reasoning"], Value::Null);
    assert_eq!(messages["items"][0]["usage"], Value::Null);

    let search = authorized_get(
        app.clone(),
        "/api/v1/sessions?profileId=default&q=%25_%20NEAR",
    )
    .await;
    assert_eq!(search.status(), StatusCode::OK);
    let search = json_body(search).await;
    assert_eq!(search["items"][0]["id"], session_id);
    assert_eq!(search["items"][0]["match"]["field"], "message");

    let malformed_absent = app
        .clone()
        .oneshot(
            Request::delete("/api/v1/sessions/missing-session")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::IF_MATCH, "not-an-etag")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        malformed_absent,
        StatusCode::BAD_REQUEST,
        "invalid_if_match",
    )
    .await;
    assert_eq!(
        authorized_delete(app.clone(), "/api/v1/sessions/missing-session")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );

    let missing_delete_precondition = authorized_delete(app.clone(), &location).await;
    assert_problem(
        missing_delete_precondition,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    )
    .await;
    let current = authorized_get(app.clone(), &location).await;
    let current_etag = current.headers()[header::ETAG].to_str().unwrap().to_owned();
    let deleted = app
        .clone()
        .oneshot(
            Request::delete(&location)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::IF_MATCH, &current_etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        authorized_delete(app.clone(), &location).await.status(),
        StatusCode::NO_CONTENT
    );

    let gone = app
        .oneshot(
            Request::post("/api/v1/sessions")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "create-contract-session")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(gone, StatusCode::GONE, "idempotent_resource_deleted").await;
}

const RESERVED_RUN_ID: &str = "run_0123456789abcdef0123456789abcdef";
const RESERVED_APPROVAL_ID: &str = "approval_0123456789abcdef0123456789abcdef";
const RESERVED_CLARIFICATION_ID: &str = "clarification_0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn run_actions_require_authentication_before_contract_validation() {
    let (app, _home) = app();
    let response = run_action_request(
        app,
        "/api/v1/runs/not-a-run/approvals/not-an-approval",
        None,
        "{not-json",
        false,
    )
    .await;

    assert_problem(response, StatusCode::UNAUTHORIZED, "unauthorized").await;
}

#[tokio::test]
async fn run_actions_validate_their_resource_ids_first() {
    let (app, _home) = app();
    let invalid_paths = [
        "/api/v1/runs/run_0123456789ABCDEF0123456789abcdef/approvals/approval_0123456789abcdef0123456789abcdef",
        "/api/v1/runs/run_0123456789abcdef0123456789abcde/approvals/approval_0123456789abcdef0123456789abcdef",
        "/api/v1/runs/run_0123456789abcdef0123456789abcdef/approvals/action_0123456789abcdef0123456789abcdef",
        "/api/v1/runs/run_0123456789abcdef0123456789abcdef/approvals/approval_0123456789abcdef0123456789abcde",
    ];
    for path in invalid_paths {
        let response = run_action_request(app.clone(), path, None, "{not-json", true).await;
        assert_problem(response, StatusCode::BAD_REQUEST, "validation_failed").await;
    }

    let invalid_clarification = format!(
        "/api/v1/runs/{RESERVED_RUN_ID}/clarifications/request_0123456789abcdef0123456789abcdef"
    );
    let response = run_action_request(app, &invalid_clarification, None, "{not-json", true).await;
    assert_problem(response, StatusCode::BAD_REQUEST, "validation_failed").await;
}

#[tokio::test]
async fn run_actions_require_application_json() {
    let (app, _home) = app();
    let approval_path = format!("/api/v1/runs/{RESERVED_RUN_ID}/approvals/{RESERVED_APPROVAL_ID}");
    for content_type in [None, Some("text/plain"), Some("application/problem+json")] {
        let response = run_action_request(
            app.clone(),
            &approval_path,
            content_type,
            r#"{"decision":"once"}"#,
            true,
        )
        .await;
        assert_problem(
            response,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        )
        .await;
    }

    let duplicate = app
        .oneshot(
            Request::post(&approval_path)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(r#"{"decision":"once"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        duplicate,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;
}

#[tokio::test]
async fn approval_body_is_strict_and_bounded() {
    let (app, _home) = app();
    let path = format!("/api/v1/runs/{RESERVED_RUN_ID}/approvals/{RESERVED_APPROVAL_ID}");
    for body in [
        "{not-json",
        r#"{"decision":"once","extra":true}"#,
        r#"{"decision":"later"}"#,
        r#"{"decision":"once","reason":1}"#,
    ] {
        let response =
            run_action_request(app.clone(), &path, Some("application/json"), body, true).await;
        assert_problem(response, StatusCode::BAD_REQUEST, "invalid_json").await;
    }

    let too_long_reason = serde_json::json!({
        "decision": "deny",
        "reason": "x".repeat(2_001),
    })
    .to_string();
    let response = run_action_request(
        app.clone(),
        &path,
        Some("application/json"),
        &too_long_reason,
        true,
    )
    .await;
    assert_problem(response, StatusCode::BAD_REQUEST, "validation_failed").await;

    let oversized = format!(
        "{{\"decision\":\"once\",\"reason\":\"{}\"}}",
        "x".repeat(128 * 1024)
    );
    let response = run_action_request(app, &path, Some("application/json"), &oversized, true).await;
    assert_problem(response, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").await;
}

#[tokio::test]
async fn valid_approval_decisions_reach_the_store_boundary() {
    let (app, _home) = app();
    let path = format!("/api/v1/runs/{RESERVED_RUN_ID}/approvals/{RESERVED_APPROVAL_ID}");
    for body in [
        r#"{"decision":"once"}"#,
        r#"{"decision":"session","reason":null}"#,
        r#"{"decision":"always","reason":"remember"}"#,
        r#"{"decision":"deny","reason":""}"#,
    ] {
        let response = run_action_request(
            app.clone(),
            &path,
            Some("Application/JSON; charset=utf-8"),
            body,
            true,
        )
        .await;
        assert_problem(response, StatusCode::NOT_FOUND, "approval_not_found").await;
    }
}

#[tokio::test]
async fn clarification_body_is_strict_bounded_and_not_trimmed() {
    let (app, _home) = app();
    let path = format!("/api/v1/runs/{RESERVED_RUN_ID}/clarifications/{RESERVED_CLARIFICATION_ID}");
    for body in [
        "{not-json",
        r#"{"answer":"yes","extra":true}"#,
        r#"{"answer":1}"#,
    ] {
        let response =
            run_action_request(app.clone(), &path, Some("application/json"), body, true).await;
        assert_problem(response, StatusCode::BAD_REQUEST, "invalid_json").await;
    }

    for answer in [String::new(), "界".repeat(10_001)] {
        let body = serde_json::json!({ "answer": answer }).to_string();
        let response =
            run_action_request(app.clone(), &path, Some("application/json"), &body, true).await;
        assert_problem(response, StatusCode::BAD_REQUEST, "validation_failed").await;
    }

    for answer in ["yes".to_owned(), "   ".to_owned(), "界".repeat(10_000)] {
        let body = serde_json::json!({ "answer": answer }).to_string();
        let response =
            run_action_request(app.clone(), &path, Some("application/json"), &body, true).await;
        assert_problem(response, StatusCode::NOT_FOUND, "clarification_not_found").await;
    }
}

async fn run_action_request(
    app: axum::Router,
    path: &str,
    content_type: Option<&str>,
    body: &str,
    authorized: bool,
) -> Response<Body> {
    let mut request = Request::post(path);
    if authorized {
        request = request.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    app.oneshot(request.body(Body::from(body.to_owned())).unwrap())
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

async fn authorized_delete(app: axum::Router, path: &str) -> Response<Body> {
    app.oneshot(
        Request::delete(path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert!(response.headers().contains_key("x-request-id"));
    let body = json_body(response).await;
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code);
    assert!(
        body["requestId"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
