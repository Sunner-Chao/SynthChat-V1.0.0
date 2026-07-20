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
const PATH: &str = "/api/v1/profiles/default/workspaces";

fn app() -> (Router, TempDir) {
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

#[tokio::test]
async fn workspace_paths_are_write_only_idempotent_and_never_deleted() {
    let (app, home) = app();
    let unauthorized = app
        .clone()
        .oneshot(Request::get(PATH).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let root = home.path().join("workspace root");
    std::fs::create_dir(&root).unwrap();
    let root_text = root.to_str().unwrap();
    let created = request(
        &app,
        Request::post(PATH)
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "workspace-key")
            .body(Body::from(json!({"path": root_text}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = body(created).await;
    assert!(created["id"].as_str().unwrap().starts_with("workspace_"));
    assert_eq!(created["profileId"], "default");
    assert_eq!(created["displayName"], "workspace root");
    assert_eq!(created["available"], true);
    assert!(!created.to_string().contains(root_text));

    let replay = request(
        &app,
        Request::post(PATH)
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "workspace-key")
            .body(Body::from(json!({"path": root_text}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(body(replay).await, created);

    let other = home.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let conflict = request(
        &app,
        Request::post(PATH)
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "workspace-key")
            .body(Body::from(
                json!({"path": other.to_str().unwrap()}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_problem(conflict, StatusCode::CONFLICT, "idempotency_conflict").await;

    let listed = request(&app, Request::get(PATH).body(Body::empty()).unwrap()).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body(listed).await;
    assert_eq!(listed.as_array().unwrap(), std::slice::from_ref(&created));
    assert!(!listed.to_string().contains(root_text));

    let id = created["id"].as_str().unwrap();
    let deleted = request(
        &app,
        Request::delete(format!("{PATH}/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(root.is_dir());
}

#[tokio::test]
async fn workspace_registration_rejects_relative_missing_and_unknown_fields_without_echo() {
    let (app, _home) = app();
    for (index, payload) in [
        json!({"path": "relative/path"}),
        json!({"path": "Z:/definitely/missing/root"}),
    ]
    .into_iter()
    .enumerate()
    {
        let response = request(
            &app,
            Request::post(PATH)
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", format!("invalid-workspace-{index}"))
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await;
        assert_problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "workspace_unavailable",
        )
        .await;
    }
    let extra = request(
        &app,
        Request::post(PATH)
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "extra-key")
            .body(Body::from(
                json!({"path": "C:/", "debug": true}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_problem(extra, StatusCode::BAD_REQUEST, "invalid_json").await;
}

async fn request(app: &Router, mut request: Request<Body>) -> Response<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    app.clone().oneshot(request).await.unwrap()
}

async fn body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_problem(response: Response<Body>, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let value = body(response).await;
    assert_eq!(value["status"], status.as_u16());
    assert_eq!(value["code"], code);
    assert!(!value.to_string().contains("Z:/definitely"));
}
