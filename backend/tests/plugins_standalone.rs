use std::{fs, path::Path};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn app() -> (Router, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let profiles = ProfileService::without_credential_store(home.path().to_owned());
    (
        build_router(AppConfig::new(TOKEN.to_owned(), Vec::new(), profiles)),
        home,
    )
}

fn write_plugin(home: &Path, id: &str, extra: &str) -> std::path::PathBuf {
    let directory = home.join(".synthchat").join("plugins").join(id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("plugin.json"),
        format!(
            r#"{{
  "id": "{id}",
  "name": "Local tools",
  "version": "1.2.0",
  "description": "Manifest-only local tools.",
  "author": "SynthChat",
  "providedTools": ["local.search"],
  "requiresEnv": ["LOCAL_PLUGIN_TOKEN"]{extra}
}}"#,
        ),
    )
    .unwrap();
    directory
}

#[tokio::test]
async fn local_plugin_catalog_registers_toggles_and_removes_metadata() {
    let (app, home) = app();
    let source = write_plugin(home.path(), "local-tools", "");

    let empty = authorized(
        &app,
        Request::get("/api/v1/plugins").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(empty.headers()[header::ETAG], "\"plugin-catalog-0\"");
    assert_eq!(empty.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(body(empty).await, json!({ "items": [] }));

    let installed = authorized(
        &app,
        Request::post("/api/v1/plugins/install")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "sourcePath": "local-tools" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(installed.status(), StatusCode::CREATED);
    assert_eq!(installed.headers()[header::ETAG], "\"plugin-catalog-1\"");
    let installed = body(installed).await;
    assert_eq!(installed["id"], "local-tools");
    assert_eq!(installed["enabled"], false);
    assert_eq!(installed["execution"], "manifestOnly");
    assert!(installed.get("entryPoint").is_none());

    let enabled = authorized(
        &app,
        Request::patch("/api/v1/plugins/local-tools")
            .header(header::CONTENT_TYPE, "application/merge-patch+json")
            .header(header::IF_MATCH, "\"plugin-catalog-1\"")
            .body(Body::from(json!({ "enabled": true }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(enabled.status(), StatusCode::OK);
    assert_eq!(enabled.headers()[header::ETAG], "\"plugin-catalog-2\"");
    assert_eq!(body(enabled).await["enabled"], true);

    let stale = authorized(
        &app,
        Request::patch("/api/v1/plugins/local-tools")
            .header(header::CONTENT_TYPE, "application/merge-patch+json")
            .header(header::IF_MATCH, "\"plugin-catalog-1\"")
            .body(Body::from(json!({ "enabled": false }).to_string()))
            .unwrap(),
    )
    .await;
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    let removed = authorized(
        &app,
        Request::delete("/api/v1/plugins/local-tools")
            .header(header::IF_MATCH, "\"plugin-catalog-2\"")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(removed.headers()[header::ETAG], "\"plugin-catalog-3\"");
    assert!(source.exists());
    assert!(source.join("plugin.json").exists());
}

#[tokio::test]
async fn local_plugin_catalog_rejects_outside_and_executable_manifests() {
    let (app, home) = app();
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("plugin.json"), "{}").unwrap();

    let outside_response = authorized(
        &app,
        Request::post("/api/v1/plugins/install")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "sourcePath": outside.to_string_lossy() }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_problem(
        outside_response,
        StatusCode::BAD_REQUEST,
        "plugin_validation_failed",
    )
    .await;

    write_plugin(
        home.path(),
        "legacy-runtime",
        ",\n  \"entryPoint\": \"plugin.py\"",
    );
    let executable = authorized(
        &app,
        Request::post("/api/v1/plugins/install")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "sourcePath": "legacy-runtime" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_problem(
        executable,
        StatusCode::UNPROCESSABLE_ENTITY,
        "plugin_manifest_invalid",
    )
    .await;
}

async fn authorized(app: &Router, mut request: Request<Body>) -> Response<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    app.clone().oneshot(request).await.unwrap()
}

async fn body(response: Response<Body>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    let payload = body(response).await;
    assert_eq!(payload["code"], code);
}
