use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, WebRuntimeConfig, build_router};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const WEB_PATH: &str = "/api/v1/profiles/default/web";

fn app() -> (axum::Router, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
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
async fn provider_catalog_and_capabilities_are_authenticated_and_exact() {
    let (app, _home) = app();
    let unauthorized = app
        .clone()
        .oneshot(
            Request::get("/api/v1/web/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let providers = json_body(authorized_get(app.clone(), "/api/v1/web/providers").await).await;
    assert_eq!(
        providers,
        json!([{
            "id": "tavily",
            "displayName": "Tavily",
            "supportsSearch": true,
            "supportsExtract": true,
            "secretNames": ["TAVILY_API_KEY"],
            "defaultBaseUrl": "https://api.tavily.com",
            "customEndpointSupported": false
        }])
    );

    let capabilities = json_body(authorized_get(app, "/api/v1/capabilities").await).await;
    assert_eq!(capabilities["extensions"]["webSearch"], true);
    assert_eq!(capabilities["extensions"]["webExtract"], true);
    assert!(capabilities["extensions"]["browserAutomation"].is_boolean());
    assert_eq!(
        capabilities["extensions"]["browserAutomation"],
        capabilities["extensions"]["browserCdp"]
    );
    assert_eq!(
        capabilities["extensions"]["browserDownloads"],
        capabilities["extensions"]["browserAutomation"]
    );
}

#[tokio::test]
async fn provider_catalog_reports_the_injected_runtime_endpoint() {
    let home = tempfile::tempdir().unwrap();
    let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    let runtime =
        WebRuntimeConfig::from_tavily_base_url("https://web-gateway.example.test/providers/tavily")
            .unwrap();
    let app = build_router(
        AppConfig::new(
            TOKEN.to_owned(),
            vec!["tauri://localhost".parse().unwrap()],
            profiles,
        )
        .with_web_runtime_config(runtime),
    );

    let providers = json_body(authorized_get(app, "/api/v1/web/providers").await).await;
    assert_eq!(
        providers[0]["defaultBaseUrl"],
        "https://web-gateway.example.test/providers/tavily"
    );
    assert_eq!(providers[0]["customEndpointSupported"], false);
}

#[tokio::test]
async fn web_config_uses_shared_revision_and_keychain_readiness() {
    let (app, home) = app();
    let initial = authorized_get(app.clone(), WEB_PATH).await;
    assert_eq!(initial.status(), StatusCode::OK);
    let initial_etag = initial.headers()[header::ETAG].to_str().unwrap().to_owned();
    let initial_body = json_body(initial).await;
    assert_eq!(initial_body["revision"], unquote_etag(&initial_etag));
    assert_eq!(initial_body["sharedProvider"], Value::Null);
    assert_eq!(initial_body["searchProvider"], Value::Null);
    assert_eq!(initial_body["extractProvider"], Value::Null);
    assert_eq!(initial_body["extractCharLimit"], 15_000);
    assert_eq!(initial_body["effectiveSearch"]["status"], "unconfigured");
    assert_eq!(initial_body["effectiveExtract"]["status"], "unconfigured");

    let no_op = app
        .clone()
        .oneshot(web_patch(&initial_etag, "{}"))
        .await
        .unwrap();
    assert_eq!(no_op.status(), StatusCode::OK);
    assert_eq!(no_op.headers()[header::ETAG], initial_etag);
    assert!(!home.path().join("config.yaml").exists());

    let configured = app
        .clone()
        .oneshot(web_patch(
            &initial_etag,
            r#"{"sharedProvider":"tavily","extractCharLimit":22000}"#,
        ))
        .await
        .unwrap();
    assert_eq!(configured.status(), StatusCode::OK);
    let configured_etag = configured.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(configured_etag, initial_etag);
    let configured_body = json_body(configured).await;
    assert_eq!(configured_body["revision"], unquote_etag(&configured_etag));
    assert_eq!(
        configured_body["effectiveSearch"]["status"],
        "missingSecret"
    );
    assert_eq!(
        configured_body["effectiveExtract"]["missingSecretNames"],
        json!(["TAVILY_API_KEY"])
    );

    let secret = app
        .clone()
        .oneshot(authorized(
            Request::put("/api/v1/profiles/default/secrets/TAVILY_API_KEY")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"value":"tvly-test-secret"}"#))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(secret.status(), StatusCode::OK);
    assert_eq!(json_body(secret).await["configured"], true);

    let ready = authorized_get(app.clone(), WEB_PATH).await;
    assert_eq!(ready.headers()[header::ETAG], configured_etag);
    let ready = json_body(ready).await;
    assert_eq!(ready["effectiveSearch"]["status"], "ready");
    assert_eq!(ready["effectiveExtract"]["status"], "ready");
    assert_eq!(ready["revision"], unquote_etag(&configured_etag));
    assert!(!ready.to_string().contains("tvly-test-secret"));

    let profile_config = authorized_get(app, "/api/v1/profiles/default/config").await;
    assert_eq!(profile_config.headers()[header::ETAG], configured_etag);
    let yaml = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
    assert!(yaml.contains("backend: tavily"));
    assert!(yaml.contains("extract_char_limit: 22000"));
    assert!(!yaml.contains("tvly-test-secret"));
}

#[tokio::test]
async fn web_patch_is_strict_conditional_and_preserves_unknown_yaml() {
    let (app, home) = app();
    std::fs::write(
        home.path().join("config.yaml"),
        "unknown:\n  nested: 42\nweb:\n  backend: exa\n  search_backend: ''\n",
    )
    .unwrap();
    let initial = authorized_get(app.clone(), WEB_PATH).await;
    let etag = initial.headers()[header::ETAG].to_str().unwrap().to_owned();
    let body = json_body(initial).await;
    assert_eq!(body["sharedProvider"], "exa");
    assert_eq!(body["effectiveSearch"]["providerId"], "exa");
    assert_eq!(body["effectiveSearch"]["status"], "unsupported");

    for invalid in [
        r#"{"sharedProvider":"exa"}"#,
        r#"{"extractCharLimit":1999}"#,
        r#"{"extractCharLimit":500001}"#,
    ] {
        let response = app
            .clone()
            .oneshot(web_patch(&etag, invalid))
            .await
            .unwrap();
        assert_problem(response, StatusCode::BAD_REQUEST, "validation_failed").await;
    }
    for invalid in [
        r#"{"extra":true}"#,
        r#"{"extractCharLimit":null}"#,
        r#"{"baseUrl":"https://profile-override.example.test"}"#,
    ] {
        let response = app
            .clone()
            .oneshot(web_patch(&etag, invalid))
            .await
            .unwrap();
        assert_problem(response, StatusCode::BAD_REQUEST, "invalid_json").await;
    }

    let updated = app
        .clone()
        .oneshot(web_patch(
            &etag,
            r#"{"sharedProvider":null,"searchProvider":"tavily"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated_etag = updated.headers()[header::ETAG].to_str().unwrap().to_owned();
    let updated_body = json_body(updated).await;
    assert_eq!(updated_body["sharedProvider"], Value::Null);
    assert_eq!(updated_body["searchProvider"], "tavily");
    assert_eq!(updated_body["effectiveSearch"]["status"], "missingSecret");
    let yaml = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
    assert!(yaml.contains("unknown:"));
    assert!(yaml.contains("nested: 42"));

    let stale = app
        .clone()
        .oneshot(web_patch(&etag, r#"{"extractCharLimit":2000}"#))
        .await
        .unwrap();
    assert_eq!(stale.headers()[header::ETAG], updated_etag);
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    let missing_if_match = app
        .clone()
        .oneshot(authorized(
            Request::patch(WEB_PATH)
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .body(Body::from("{}"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_problem(
        missing_if_match,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    )
    .await;

    let wrong_content_type = app
        .oneshot(authorized(
            Request::patch(WEB_PATH)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::IF_MATCH, updated_etag)
                .body(Body::from("{}"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_problem(
        wrong_content_type,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;
}

#[tokio::test]
async fn web_readiness_fails_closed_when_keychain_is_unavailable() {
    let home = tempfile::tempdir().unwrap();
    let profiles = ProfileService::without_credential_store(home.path().to_owned());
    let app = build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        profiles,
    ));

    let response = authorized_get(app, WEB_PATH).await;
    assert_problem(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "secret_storage_unavailable",
    )
    .await;
}

fn web_patch(etag: &str, body: &str) -> Request<Body> {
    authorized(
        Request::patch(WEB_PATH)
            .header(
                header::CONTENT_TYPE,
                "application/merge-patch+json; charset=utf-8",
            )
            .header(header::IF_MATCH, etag)
            .body(Body::from(body.to_owned()))
            .unwrap(),
    )
}

fn authorized(mut request: Request<Body>) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    request
}

async fn authorized_get(app: axum::Router, path: &str) -> Response<Body> {
    app.oneshot(authorized(Request::get(path).body(Body::empty()).unwrap()))
        .await
        .unwrap()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let body = json_body(response).await;
    assert_eq!(body["code"], code);
    assert!(!body.to_string().contains("tvly-test-secret"));
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn unquote_etag(etag: &str) -> &str {
    etag.strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap()
}
