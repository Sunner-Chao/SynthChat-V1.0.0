use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use rusqlite::Connection;
use serde_json::Value;
use synthchat_hermes_backend::{
    AppConfig, ProfileService, build_router, sessions::SESSION_SCHEMA_VERSION,
};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const V21_FIXTURE: &str = include_str!("fixtures/hermes_v21.sql");

fn router(home: &TempDir) -> axum::Router {
    build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        ProfileService::without_credential_store(home.path().to_owned()),
    ))
}

fn install_source(home: &TempDir) {
    let path = home.path().join("state.db");
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    Connection::open(path)
        .unwrap()
        .execute_batch(V21_FIXTURE)
        .unwrap();
}

#[tokio::test]
async fn preview_policy_import_and_cross_restart_replay_match_the_contract() {
    let home = tempfile::tempdir().unwrap();
    install_source(&home);
    let app = router(&home);

    let capabilities = json_body(get(app.clone(), "/api/v1/capabilities").await).await;
    assert_eq!(
        capabilities["sessionStorage"]["schemaVersion"],
        SESSION_SCHEMA_VERSION
    );
    assert_eq!(
        capabilities["sessionStorage"]["hermesImportAvailable"],
        true
    );

    let preview_response = get(
        app.clone(),
        "/api/v1/profiles/default/session-imports/hermes-v21",
    )
    .await;
    assert_eq!(preview_response.status(), StatusCode::OK);
    let preview = json_body(preview_response).await;
    assert_eq!(preview["state"], "ready");
    assert_eq!(preview["sessionCount"], 1);
    assert_eq!(preview["messageCount"], 5);
    assert_eq!(preview["attachmentCount"], 3);
    assert_eq!(preview["rewoundMessageCount"], 1);
    let fingerprint = preview["snapshotFingerprint"].as_str().unwrap();
    assert_eq!(fingerprint.len(), 64);
    assert!(
        !preview
            .to_string()
            .contains(home.path().to_string_lossy().as_ref())
    );

    let denied = post_import(app.clone(), "import-http-denied", fingerprint, false).await;
    assert_problem(
        denied,
        StatusCode::UNPROCESSABLE_ENTITY,
        "hermes_attachments_require_policy",
    )
    .await;

    let imported = post_import(app.clone(), "import-http-success", fingerprint, true).await;
    assert_eq!(imported.status(), StatusCode::OK);
    let imported = json_body(imported).await;
    assert_eq!(imported["disposition"], "imported");
    assert_eq!(imported["importedSessionCount"], 1);
    assert_eq!(imported["importedMessageCount"], 5);
    assert_eq!(imported["omittedAttachmentCount"], 3);

    let sessions = json_body(get(app, "/api/v1/sessions?profileId=default").await).await;
    assert_eq!(sessions["items"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["items"][0]["source"], "hermes-agent:v21");

    std::fs::remove_file(home.path().join("state.db")).unwrap();
    let restarted = router(&home);
    let replay = post_import(restarted.clone(), "import-http-success", fingerprint, true).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await["disposition"], "replayed");

    let absent = get(
        restarted.clone(),
        "/api/v1/profiles/default/session-imports/hermes-v21",
    )
    .await;
    assert_eq!(absent.status(), StatusCode::OK);
    assert_eq!(json_body(absent).await["state"], "absent");
    let missing = post_import(restarted, "import-http-new-key", fingerprint, true).await;
    assert_problem(missing, StatusCode::NOT_FOUND, "hermes_state_not_found").await;
}

#[tokio::test]
async fn changed_source_and_target_conflicts_are_bounded_and_atomic() {
    let home = tempfile::tempdir().unwrap();
    install_source(&home);
    let app = router(&home);
    let preview = json_body(
        get(
            app.clone(),
            "/api/v1/profiles/default/session-imports/hermes-v21",
        )
        .await,
    )
    .await;
    let fingerprint = preview["snapshotFingerprint"].as_str().unwrap().to_owned();

    let source = Connection::open(home.path().join("state.db")).unwrap();
    source
        .execute(
            "UPDATE sessions SET title = 'Changed after preview' WHERE id = 'synthetic-session-1'",
            [],
        )
        .unwrap();
    drop(source);
    let changed = post_import(app.clone(), "import-source-changed", &fingerprint, true).await;
    assert_problem(
        changed,
        StatusCode::CONFLICT,
        "hermes_import_source_changed",
    )
    .await;
    let sessions = json_body(get(app.clone(), "/api/v1/sessions?profileId=default").await).await;
    assert!(sessions["items"].as_array().unwrap().is_empty());

    install_source(&home);
    let imported = post_import(app.clone(), "import-conflict-first", &fingerprint, true).await;
    assert_eq!(imported.status(), StatusCode::OK);
    let target = Connection::open(home.path().join(".synthchat/sessions-v1.db")).unwrap();
    target
        .execute(
            "UPDATE sessions SET revision = 'session_rev_locally_changed'",
            [],
        )
        .unwrap();
    drop(target);

    let conflict = post_import(app, "import-conflict-second", &fingerprint, true).await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let conflict = json_body(conflict).await;
    assert_eq!(conflict["code"], "hermes_import_conflict");
    assert_eq!(conflict["conflictCount"], 1);
    assert_eq!(conflict["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(conflict["conflicts"][0]["code"], "targetModified");
    assert_eq!(conflict["conflictsDropped"], 0);
}

#[tokio::test]
async fn unsupported_or_non_file_sources_fail_without_leaking_paths() {
    let unsupported_home = tempfile::tempdir().unwrap();
    install_source(&unsupported_home);
    Connection::open(unsupported_home.path().join("state.db"))
        .unwrap()
        .execute("UPDATE schema_version SET version = 20", [])
        .unwrap();
    let unsupported = get(
        router(&unsupported_home),
        "/api/v1/profiles/default/session-imports/hermes-v21",
    )
    .await;
    assert_problem(
        unsupported,
        StatusCode::UNPROCESSABLE_ENTITY,
        "hermes_schema_unsupported",
    )
    .await;

    let directory_home = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory_home.path().join("state.db")).unwrap();
    let invalid = get(
        router(&directory_home),
        "/api/v1/profiles/default/session-imports/hermes-v21",
    )
    .await;
    let status = invalid.status();
    let body = json_body(invalid).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "hermes_import_source_invalid");
    assert!(
        !body
            .to_string()
            .contains(directory_home.path().to_string_lossy().as_ref())
    );
}

async fn get(app: axum::Router, path: &str) -> Response<Body> {
    app.oneshot(
        Request::get(path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn post_import(
    app: axum::Router,
    key: &str,
    fingerprint: &str,
    allow_attachment_omission: bool,
) -> Response<Body> {
    app.oneshot(
        Request::post("/api/v1/profiles/default/session-imports/hermes-v21")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(
                serde_json::json!({
                    "expectedSnapshotFingerprint": fingerprint,
                    "allowAttachmentOmission": allow_attachment_omission,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    let body = json_body(response).await;
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code);
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
