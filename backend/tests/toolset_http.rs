use std::{collections::BTreeSet, sync::Arc};

use axum::{
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const TOOLSETS_PATH: &str = "/api/v1/profiles/default/toolsets";

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
async fn catalog_is_authenticated_versioned_and_capability_gated() {
    let (app, _home) = app();
    let unauthorized = app
        .clone()
        .oneshot(Request::get(TOOLSETS_PATH).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_problem(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let capabilities = authorized_get(app.clone(), "/api/v1/capabilities").await;
    assert_eq!(capabilities.status(), StatusCode::OK);
    let capabilities = json_body(capabilities).await;
    let code_execution_available = capabilities["extensions"]["codeExecution"]
        .as_bool()
        .expect("codeExecution must be a stable boolean capability");
    let browser_available = capabilities["extensions"]["browserAutomation"]
        .as_bool()
        .expect("browserAutomation must be a stable boolean capability");
    assert_eq!(capabilities["extensions"]["browserCdp"], browser_available);
    assert_eq!(capabilities["extensions"]["toolsetManagement"], true);
    assert_eq!(capabilities["extensions"]["toolExecution"], true);
    assert_eq!(capabilities["extensions"]["workspaceManagement"], true);
    assert_eq!(capabilities["engine"]["features"]["toolProgress"], true);
    assert_eq!(capabilities["engine"]["features"]["approvals"], true);
    assert_eq!(
        capabilities["engine"]["features"]["asyncToolDelivery"],
        true
    );

    let response = authorized_get(app.clone(), TOOLSETS_PATH).await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"'));
    let catalog = json_body(response).await;
    let catalog = catalog.as_array().unwrap();
    assert_eq!(catalog.len(), 25);
    let ids: BTreeSet<_> = catalog
        .iter()
        .map(|toolset| toolset["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), catalog.len());
    assert!(ids.contains("web"));
    assert!(ids.contains("computer_use"));
    assert_eq!(
        catalog
            .iter()
            .find(|toolset| toolset["id"] == "code_execution")
            .unwrap()["configured"],
        code_execution_available
    );
    assert_eq!(
        catalog
            .iter()
            .find(|toolset| toolset["id"] == "browser")
            .unwrap()["configured"],
        browser_available
    );
    assert_eq!(
        catalog
            .iter()
            .find(|toolset| toolset["id"] == "memory")
            .unwrap()["configured"],
        true
    );
    assert_eq!(
        catalog
            .iter()
            .find(|toolset| toolset["id"] == "session_search")
            .unwrap()["configured"],
        true
    );
    assert!(
        catalog
            .iter()
            .filter(|toolset| toolset["configured"] == true)
            .all(|toolset| matches!(
                toolset["id"].as_str(),
                Some("browser" | "code_execution" | "memory" | "session_search")
            ))
    );
    assert!(catalog.iter().all(|toolset| {
        let keys: BTreeSet<_> = toolset
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys == BTreeSet::from([
            "configured",
            "description",
            "displayName",
            "enabled",
            "id",
            "tools",
        ])
    }));

    let config = authorized_get(app, "/api/v1/profiles/default/config").await;
    assert_eq!(config.headers()[header::ETAG], etag);
}

#[tokio::test]
async fn toggle_uses_profile_config_etag_and_preserves_no_op_revision() {
    let (app, home) = app();
    let initial = authorized_get(app.clone(), TOOLSETS_PATH).await;
    let initial_etag = initial.headers()[header::ETAG].to_str().unwrap().to_owned();

    let initial_no_op = app
        .clone()
        .oneshot(toolset_patch(
            "terminal",
            &initial_etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(initial_no_op.status(), StatusCode::OK);
    assert_eq!(initial_no_op.headers()[header::ETAG], initial_etag);
    assert!(!home.path().join("config.yaml").exists());

    let enabled = app
        .clone()
        .oneshot(toolset_patch(
            "terminal",
            &initial_etag,
            r#"{"enabled":true}"#,
        ))
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    let enabled_etag = enabled.headers()[header::ETAG].to_str().unwrap().to_owned();
    assert_ne!(enabled_etag, initial_etag);
    let enabled_body = json_body(enabled).await;
    assert_eq!(enabled_body["id"], "terminal");
    assert_eq!(enabled_body["enabled"], true);
    assert_eq!(enabled_body["configured"], false);

    let listed = authorized_get(app.clone(), TOOLSETS_PATH).await;
    assert_eq!(listed.headers()[header::ETAG], enabled_etag);
    let listed = json_body(listed).await;
    assert_eq!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|toolset| toolset["id"] == "terminal")
            .unwrap()["enabled"],
        true
    );

    let no_op = app
        .clone()
        .oneshot(toolset_patch(
            "terminal",
            &enabled_etag,
            r#"{"enabled":true}"#,
        ))
        .await
        .unwrap();
    assert_eq!(no_op.status(), StatusCode::OK);
    assert_eq!(no_op.headers()[header::ETAG], enabled_etag);

    let stale = app
        .clone()
        .oneshot(toolset_patch(
            "terminal",
            &initial_etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(stale.headers()[header::ETAG], enabled_etag);
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    let disabled = app
        .clone()
        .oneshot(toolset_patch(
            "terminal",
            &enabled_etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled_etag = disabled.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(disabled_etag, enabled_etag);
    assert_eq!(json_body(disabled).await["enabled"], false);

    let config = authorized_get(app, "/api/v1/profiles/default/config").await;
    assert_eq!(config.headers()[header::ETAG], disabled_etag);
    let persisted = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
    assert!(persisted.contains("disabled_toolsets:"));
    assert!(persisted.contains("terminal"));
}

#[tokio::test]
async fn patch_rejects_invalid_headers_bodies_and_resources() {
    let (app, _home) = app();
    let listed = authorized_get(app.clone(), TOOLSETS_PATH).await;
    let etag = listed.headers()[header::ETAG].to_str().unwrap().to_owned();

    let missing_content_type = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/default/toolsets/web")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::IF_MATCH, &etag)
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        missing_content_type,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;

    let missing_precondition = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/default/toolsets/web")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .body(Body::from(r#"{"enabled":true}"#))
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

    for invalid in ["not-an-etag", "*", &format!("W/{etag}")] {
        let response = app
            .clone()
            .oneshot(toolset_patch("web", invalid, r#"{"enabled":true}"#))
            .await
            .unwrap();
        assert_problem(response, StatusCode::BAD_REQUEST, "invalid_if_match").await;
    }

    let mut duplicate = toolset_patch("web", &etag, r#"{"enabled":true}"#);
    duplicate
        .headers_mut()
        .append(header::IF_MATCH, HeaderValue::from_static("\"duplicate\""));
    let duplicate = app.clone().oneshot(duplicate).await.unwrap();
    assert_problem(duplicate, StatusCode::BAD_REQUEST, "invalid_if_match").await;

    for invalid_body in [
        "{}",
        r#"{"enabled":true,"config":{}}"#,
        r#"{"enabled":"yes"}"#,
        "null",
        "{",
    ] {
        let response = app
            .clone()
            .oneshot(toolset_patch("web", &etag, invalid_body))
            .await
            .unwrap();
        assert_problem(response, StatusCode::BAD_REQUEST, "invalid_json").await;
    }

    let unknown = app
        .clone()
        .oneshot(toolset_patch(
            "not_registered",
            &etag,
            r#"{"enabled":true}"#,
        ))
        .await
        .unwrap();
    assert_problem(unknown, StatusCode::NOT_FOUND, "resource_not_found").await;

    let missing_profile = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/profiles/missing/toolsets/web")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/merge-patch+json")
                .header(header::IF_MATCH, &etag)
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(missing_profile, StatusCode::NOT_FOUND, "profile_not_found").await;

    let invalid_profile = authorized_get(app.clone(), "/api/v1/profiles/INVALID/toolsets").await;
    assert_problem(
        invalid_profile,
        StatusCode::BAD_REQUEST,
        "invalid_profile_id",
    )
    .await;

    let missing_list = authorized_get(app, "/api/v1/profiles/missing/toolsets").await;
    assert_problem(missing_list, StatusCode::NOT_FOUND, "profile_not_found").await;
}

#[tokio::test]
async fn stale_same_state_patch_conflicts_without_creating_a_noop_write() {
    let (app, home) = app();
    let listed = authorized_get(app.clone(), TOOLSETS_PATH).await;
    let initial_etag = listed.headers()[header::ETAG].to_str().unwrap().to_owned();

    let initial_no_op = app
        .clone()
        .oneshot(toolset_patch(
            "terminal",
            &initial_etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(initial_no_op.status(), StatusCode::OK);
    assert_eq!(initial_no_op.headers()[header::ETAG], initial_etag);
    assert!(!home.path().join("config.yaml").exists());

    let changed = app
        .clone()
        .oneshot(toolset_patch(
            "browser",
            &initial_etag,
            r#"{"enabled":true}"#,
        ))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let changed_etag = changed.headers()[header::ETAG].to_str().unwrap().to_owned();

    let stale_same_state = app
        .oneshot(toolset_patch(
            "terminal",
            &initial_etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(stale_same_state.headers()[header::ETAG], changed_etag);
    assert_problem(stale_same_state, StatusCode::CONFLICT, "revision_conflict").await;
}

#[tokio::test]
async fn concurrent_writers_cannot_both_commit_the_same_revision() {
    let (app, _home) = app();
    let listed = authorized_get(app.clone(), TOOLSETS_PATH).await;
    let etag = listed.headers()[header::ETAG].to_str().unwrap().to_owned();

    let first = app
        .clone()
        .oneshot(toolset_patch("terminal", &etag, r#"{"enabled":true}"#));
    let second = app
        .clone()
        .oneshot(toolset_patch("browser", &etag, r#"{"enabled":true}"#));
    let (first, second) = tokio::join!(first, second);
    let statuses = [first.unwrap().status(), second.unwrap().status()];

    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
}

fn toolset_patch(toolset_id: &str, etag: &str, body: &str) -> Request<Body> {
    Request::patch(format!("/api/v1/profiles/default/toolsets/{toolset_id}"))
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(
            header::CONTENT_TYPE,
            "application/merge-patch+json; charset=utf-8",
        )
        .header(header::IF_MATCH, etag)
        .body(Body::from(body.to_owned()))
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
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
