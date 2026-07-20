use std::{fs, sync::Arc};

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const SKILLS_PATH: &str = "/api/v1/profiles/default/skills";

fn app() -> (axum::Router, TempDir) {
    let home = tempfile::tempdir().unwrap();
    install_skill(
        &home,
        "research",
        "paper-search",
        "---\nname: paper-search\ndescription: Search research papers\nversion: 2.1.0\n---\n",
    );
    install_skill(
        &home,
        "writing",
        "editor",
        "---\nname: editorial-review\ndescription: Review and improve drafts\n---\n",
    );
    let hub = home.path().join("skills/.hub");
    fs::create_dir_all(&hub).unwrap();
    fs::write(
        hub.join("lock.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "installed": {
                "paper-search": {
                    "source": "official",
                    "identifier": "official/research/papers",
                    "content_hash": "sha256:paper-search-fixture",
                    "install_path": "research/paper-search"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        home.path().join("config.yaml"),
        "unknown:\n  preserve: true\nskills:\n  config:\n    paper-search:\n      mode: strict\n",
    )
    .unwrap();

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

fn install_skill(home: &TempDir, category: &str, folder: &str, content: &str) {
    let directory = home.path().join("skills").join(category).join(folder);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), content).unwrap();
}

#[tokio::test]
async fn list_is_authenticated_searchable_paginated_and_reports_provenance() {
    let (app, _home) = app();
    let unauthorized = app
        .clone()
        .oneshot(Request::get(SKILLS_PATH).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_problem(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let capabilities = authorized_get(app.clone(), "/api/v1/capabilities").await;
    let capabilities = json_body(capabilities).await;
    assert_eq!(capabilities["extensions"]["skillDiscovery"], true);
    assert_eq!(capabilities["extensions"]["skillEnablement"], true);
    assert_eq!(capabilities["engine"]["features"]["skillManagement"], true);

    let first = authorized_get(app.clone(), &format!("{SKILLS_PATH}?limit=1")).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()[header::ETAG].to_str().unwrap();
    assert!(
        !etag.starts_with("W/") && etag.len() > 2 && etag.starts_with('"') && etag.ends_with('"')
    );
    let first = json_body(first).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let cursor = first["nextCursor"].as_str().unwrap();

    let second = authorized_get(
        app.clone(),
        &format!("{SKILLS_PATH}?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let second = json_body(second).await;
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert!(second["nextCursor"].is_null());

    let search = authorized_get(app.clone(), &format!("{SKILLS_PATH}?q=PAPERS")).await;
    assert_eq!(search.status(), StatusCode::OK);
    let search = json_body(search).await;
    assert_eq!(search["items"].as_array().unwrap().len(), 1);
    let skill = &search["items"][0];
    assert_eq!(skill["name"], "paper-search");
    assert_eq!(skill["source"], "bundled");
    assert_eq!(skill["version"], "2.1.0");
    assert_eq!(skill["enabled"], true);
    assert_eq!(skill["uninstallable"], true);
    assert_eq!(skill["configurable"], false);
    assert!(skill.get("configSchema").is_none());

    let invalid_cursor =
        authorized_get(app, &format!("{SKILLS_PATH}?cursor=tampered&limit=1")).await;
    assert_problem(invalid_cursor, StatusCode::BAD_REQUEST, "invalid_cursor").await;
}

#[tokio::test]
async fn toggle_uses_profile_config_etag_and_preserves_nested_configuration() {
    let (app, home) = app();
    let skills = authorized_get(app.clone(), SKILLS_PATH).await;
    let initial_etag = skills.headers()[header::ETAG].to_str().unwrap().to_owned();
    let skills = json_body(skills).await;
    let skill_id = skills["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["name"] == "paper-search")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let disabled = app
        .clone()
        .oneshot(skill_patch(
            &skill_id,
            &initial_etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled_etag = disabled.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(disabled_etag, initial_etag);
    assert_eq!(json_body(disabled).await["enabled"], false);

    let relisted = json_body(authorized_get(app.clone(), SKILLS_PATH).await).await;
    assert_eq!(
        relisted["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["id"] == skill_id)
            .unwrap()["enabled"],
        false
    );
    let persisted = fs::read_to_string(home.path().join("config.yaml")).unwrap();
    assert!(persisted.contains("mode: strict"));
    assert!(persisted.contains("unknown:"));
    assert!(persisted.contains("paper-search"));

    let stale = app
        .clone()
        .oneshot(skill_patch(&skill_id, &initial_etag, r#"{"enabled":true}"#))
        .await
        .unwrap();
    assert_eq!(stale.headers()[header::ETAG], disabled_etag);
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    let enabled = app
        .oneshot(skill_patch(
            &skill_id,
            &disabled_etag,
            r#"{"enabled":true}"#,
        ))
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    assert_eq!(json_body(enabled).await["enabled"], true);
}

#[tokio::test]
async fn invalid_skill_requests_fail_without_mutating_profile_configuration() {
    let (app, home) = app();
    let skills = authorized_get(app.clone(), SKILLS_PATH).await;
    let etag = skills.headers()[header::ETAG].to_str().unwrap().to_owned();
    let before = fs::read(home.path().join("config.yaml")).unwrap();
    let skills = json_body(skills).await;
    let skill_id = skills["items"][0]["id"].as_str().unwrap();

    for body in [
        "{}",
        r#"{"config":{"mode":"unsafe"}}"#,
        r#"{"enabled":true,"unknown":1}"#,
        r#"{"enabled":"yes"}"#,
    ] {
        let response = app
            .clone()
            .oneshot(skill_patch(skill_id, &etag, body))
            .await
            .unwrap();
        assert_problem(response, StatusCode::BAD_REQUEST, expected_code(body)).await;
    }

    let unknown = app
        .clone()
        .oneshot(skill_patch(
            "skill_00000000000000000000000000000000",
            &etag,
            r#"{"enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_problem(unknown, StatusCode::NOT_FOUND, "resource_not_found").await;

    let missing_profile = authorized_get(app.clone(), "/api/v1/profiles/missing/skills").await;
    assert_problem(missing_profile, StatusCode::NOT_FOUND, "profile_not_found").await;
    let invalid_query = authorized_get(app, &format!("{SKILLS_PATH}?limit=0")).await;
    assert_problem(invalid_query, StatusCode::BAD_REQUEST, "validation_failed").await;
    assert_eq!(fs::read(home.path().join("config.yaml")).unwrap(), before);
}

fn expected_code(body: &str) -> &'static str {
    if body.contains("unknown") || body.contains("\"yes\"") {
        "invalid_json"
    } else {
        "validation_failed"
    }
}

fn skill_patch(skill_id: &str, etag: &str, body: &str) -> Request<Body> {
    Request::patch(format!("/api/v1/profiles/default/skills/{skill_id}"))
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
    let body = json_body(response).await;
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code);
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
