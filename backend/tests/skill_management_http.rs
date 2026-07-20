use std::{fs, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "skill-management-http-token";
const INSTALL_PATH: &str = "/api/v1/profiles/default/skills/install";
const SKILLS_PATH: &str = "/api/v1/profiles/default/skills";

struct Fixture {
    app: Router,
    _home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let profiles = ProfileService::without_credential_store(home.path().to_owned());
        let app = build_router(AppConfig::new(TOKEN.to_owned(), Vec::new(), profiles));
        Self { app, _home: home }
    }

    fn unavailable() -> Self {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join(".synthchat"), b"not-a-directory").unwrap();
        let profiles = ProfileService::without_credential_store(home.path().to_owned());
        let app = build_router(AppConfig::new(TOKEN.to_owned(), Vec::new(), profiles));
        Self { app, _home: home }
    }

    async fn send(&self, request: Request<Body>) -> Response<Body> {
        self.app.clone().oneshot(authorized(request)).await.unwrap()
    }

    async fn upload_skill(&self, key: &str, name: &str, content: &str) -> String {
        let boundary = "synthchat-skill-management-boundary";
        let response = self
            .send(
                Request::post("/api/v1/files")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header("Idempotency-Key", key)
                    .body(Body::from(single_file_body(
                        boundary,
                        name,
                        "text/markdown",
                        content.as_bytes(),
                    )))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await["id"].as_str().unwrap().to_owned()
    }

    async fn install(&self, key: &str, payload: Value) -> Response<Body> {
        self.send(json_request(Method::POST, INSTALL_PATH, key, payload))
            .await
    }

    async fn operation(&self, operation_id: &str) -> Response<Body> {
        self.send(
            Request::get(format!("/api/v1/operations/{operation_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn await_terminal(&self, operation_id: &str) -> Value {
        for _ in 0..200 {
            let response = self.operation(operation_id).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let operation = json_body(response).await;
            if matches!(
                operation["status"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ) {
                return operation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("operation {operation_id} did not reach a terminal state");
    }
}

#[tokio::test]
async fn install_enforces_content_type_idempotency_and_exactly_one_source() {
    let fixture = Fixture::new();

    let wrong_content_type = fixture
        .send(
            Request::post(INSTALL_PATH)
                .header(header::CONTENT_TYPE, "text/plain")
                .header("Idempotency-Key", "skill-header-key")
                .body(Body::from(r#"{"registryId":"official/example"}"#))
                .unwrap(),
        )
        .await;
    assert_problem(
        wrong_content_type,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;

    let mut duplicate_content_type = json_request(
        Method::POST,
        INSTALL_PATH,
        "skill-duplicate-content-type",
        json!({"registryId": "official/example"}),
    );
    duplicate_content_type
        .headers_mut()
        .append(header::CONTENT_TYPE, "application/json".parse().unwrap());
    assert_problem(
        fixture.send(duplicate_content_type).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;

    let missing_key = fixture
        .send(
            Request::post(INSTALL_PATH)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"registryId":"official/example"}"#))
                .unwrap(),
        )
        .await;
    assert_problem(
        missing_key,
        StatusCode::BAD_REQUEST,
        "invalid_idempotency_key",
    )
    .await;

    for (key, payload, expected_code) in [
        ("skill-empty-source", json!({}), "validation_failed"),
        (
            "skill-many-sources",
            json!({"registryId": "official/example", "url": "https://example.com/SKILL.md"}),
            "validation_failed",
        ),
        (
            "skill-bad-file-id",
            json!({"fileId": "../secret"}),
            "validation_failed",
        ),
        (
            "skill-trimmed-source",
            json!({"registryId": " official/example"}),
            "validation_failed",
        ),
        (
            "skill-unknown-field",
            json!({"registryId": "official/example", "unknown": true}),
            "invalid_json",
        ),
    ] {
        assert_problem(
            fixture.install(key, payload).await,
            StatusCode::BAD_REQUEST,
            expected_code,
        )
        .await;
    }
}

#[tokio::test]
async fn file_install_replays_idempotently_and_uninstalls_through_operations() {
    let fixture = Fixture::new();
    let file_id = fixture
        .upload_skill(
            "skill-upload-source",
            "sample-skill.md",
            "---\nname: sample-skill\ndescription: Summarize user-provided notes\nversion: 1.0.0\n---\n\n# Sample Skill\n\nSummarize the notes supplied by the user.\n",
        )
        .await;

    let accepted = fixture
        .install("skill-install-replay", json!({"fileId": file_id}))
        .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted = json_body(accepted).await;
    assert_eq!(accepted["kind"], "skillInstall");
    let operation_id = accepted["id"].as_str().unwrap().to_owned();

    let replay = fixture
        .install("skill-install-replay", json!({"fileId": file_id}))
        .await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(json_body(replay).await["id"], operation_id);

    assert_problem(
        fixture
            .install(
                "skill-install-replay",
                json!({"url": "https://127.0.0.1/SKILL.md"}),
            )
            .await,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    )
    .await;

    let completed = fixture.await_terminal(&operation_id).await;
    assert_eq!(completed["status"], "completed");
    assert!(completed.get("error").is_none());

    let skills = fixture
        .send(Request::get(SKILLS_PATH).body(Body::empty()).unwrap())
        .await;
    assert_eq!(skills.status(), StatusCode::OK);
    let skills = json_body(skills).await;
    let installed = skills["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["name"] == "sample-skill")
        .unwrap();
    assert_eq!(installed["source"], "file");
    assert_eq!(installed["uninstallable"], true);
    let skill_id = installed["id"].as_str().unwrap();

    let uninstall = fixture
        .send(
            Request::delete(format!("{SKILLS_PATH}/{skill_id}"))
                .header("Idempotency-Key", "skill-uninstall-replay")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(uninstall.status(), StatusCode::ACCEPTED);
    let uninstall = json_body(uninstall).await;
    assert_eq!(uninstall["kind"], "skillUninstall");
    let uninstall_id = uninstall["id"].as_str().unwrap();
    let running_replay = fixture
        .send(
            Request::delete(format!("{SKILLS_PATH}/{skill_id}"))
                .header("Idempotency-Key", "skill-uninstall-replay")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(running_replay.status(), StatusCode::ACCEPTED);
    assert_eq!(json_body(running_replay).await["id"], uninstall_id);
    assert_eq!(
        fixture.await_terminal(uninstall_id).await["status"],
        "completed"
    );
    let completed_replay = fixture
        .send(
            Request::delete(format!("{SKILLS_PATH}/{skill_id}"))
                .header("Idempotency-Key", "skill-uninstall-replay")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(completed_replay.status(), StatusCode::ACCEPTED);
    let completed_replay = json_body(completed_replay).await;
    assert_eq!(completed_replay["id"], uninstall_id);
    assert_eq!(completed_replay["status"], "completed");

    let skills = json_body(
        fixture
            .send(Request::get(SKILLS_PATH).body(Body::empty()).unwrap())
            .await,
    )
    .await;
    assert!(
        skills["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|skill| skill["name"] != "sample-skill")
    );
}

#[tokio::test]
async fn unsafe_url_failure_is_asynchronous_and_redacted() {
    let fixture = Fixture::new();
    let unsafe_url = "https://127.0.0.1/SKILL.md";
    let mut request = json_request(
        Method::POST,
        INSTALL_PATH,
        "skill-unsafe-url",
        json!({"url": unsafe_url}),
    );
    request
        .headers_mut()
        .insert("X-Request-Id", "skill-unsafe-origin".parse().unwrap());
    let accepted = fixture.send(request).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted = json_body(accepted).await;
    let operation_id = accepted["id"].as_str().unwrap();
    let failed = fixture.await_terminal(operation_id).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["code"], "skill_source_unsafe");
    assert_eq!(failed["error"]["requestId"], "skill-unsafe-origin");
    assert_ne!(failed["error"]["requestId"], operation_id);
    let serialized = failed.to_string();
    assert!(!serialized.contains(unsafe_url));
    assert!(!serialized.contains("127.0.0.1"));
    assert!(!serialized.contains(".synthchat"));
}

#[tokio::test]
async fn invalid_request_ids_are_replaced_before_an_operation_is_persisted() {
    let fixture = Fixture::new();
    for (index, invalid_request_id) in ["request id with spaces", "request/id", "request?id"]
        .into_iter()
        .enumerate()
    {
        let mut request = json_request(
            Method::POST,
            INSTALL_PATH,
            &format!("skill-invalid-request-id-{index}"),
            json!({"fileId": "file_00000000000000000000000000000000"}),
        );
        request
            .headers_mut()
            .insert("X-Request-Id", invalid_request_id.parse().unwrap());
        let accepted = fixture.send(request).await;
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        let normalized_request_id = accepted.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_ne!(normalized_request_id, invalid_request_id);
        assert!(
            !normalized_request_id.is_empty()
                && normalized_request_id.len() <= 128
                && normalized_request_id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
                })
        );
        let operation = json_body(accepted).await;
        assert_eq!(operation["status"], "failed");
        assert_eq!(operation["error"]["requestId"], normalized_request_id);
        assert_eq!(operation["error"]["code"], "skill_source_not_found");
    }
}

#[tokio::test]
async fn operation_lookup_rejects_invalid_and_unknown_opaque_ids() {
    let fixture = Fixture::new();
    assert_problem(
        fixture
            .send(
                Request::delete(format!(
                    "{SKILLS_PATH}/skill_00000000000000000000000000000000"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_idempotency_key",
    )
    .await;
    for request in [
        Request::patch(format!("{SKILLS_PATH}/not-a-skill"))
            .body(Body::empty())
            .unwrap(),
        Request::delete(format!("{SKILLS_PATH}/skill_ABCDEF"))
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_problem(
            fixture.send(request).await,
            StatusCode::BAD_REQUEST,
            "validation_failed",
        )
        .await;
    }
    assert_problem(
        fixture.operation("not-an-operation").await,
        StatusCode::BAD_REQUEST,
        "invalid_operation_id",
    )
    .await;
    assert_problem(
        fixture
            .operation("op_00000000000000000000000000000000")
            .await,
        StatusCode::NOT_FOUND,
        "resource_not_found",
    )
    .await;
}

#[tokio::test]
async fn unavailable_management_store_fails_closed_before_starting_operations() {
    let fixture = Fixture::unavailable();
    let capabilities = fixture
        .send(
            Request::get("/api/v1/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(capabilities.status(), StatusCode::OK);
    assert_eq!(
        json_body(capabilities).await["engine"]["features"]["skillManagement"],
        false
    );

    assert_problem(
        fixture
            .install(
                "skill-unavailable-install",
                json!({"registryId": "official/example"}),
            )
            .await,
        StatusCode::SERVICE_UNAVAILABLE,
        "skill_management_unavailable",
    )
    .await;
    assert_problem(
        fixture
            .send(
                Request::delete(format!(
                    "{SKILLS_PATH}/skill_00000000000000000000000000000000"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await,
        StatusCode::SERVICE_UNAVAILABLE,
        "skill_management_unavailable",
    )
    .await;
}

fn json_request(method: Method, path: &str, key: &str, payload: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header("Idempotency-Key", key)
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap()
}

fn authorized(mut request: Request<Body>) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    request
}

fn single_file_body(boundary: &str, name: &str, mime_type: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {mime_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
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
    assert!(
        body["requestId"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    let serialized = body.to_string();
    assert!(!serialized.contains(".synthchat"));
    assert!(!serialized.contains("127.0.0.1"));
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
