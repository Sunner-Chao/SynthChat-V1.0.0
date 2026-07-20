use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use synthchat_hermes_backend::{
    AppConfig, ProfileService, build_router, files::MAX_RETAINED_FILE_BYTES,
};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "file-http-token";
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
type MultipartPart<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

struct Fixture {
    home: TempDir,
    app: Router,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let profiles = ProfileService::without_credential_store(home.path().to_owned());
        let app = build_router(AppConfig::new(TOKEN.to_owned(), Vec::new(), profiles));
        Self { home, app }
    }

    async fn upload(&self, key: &str, name: &str, mime_type: &str, bytes: &[u8]) -> Response<Body> {
        let boundary = "synthchat-file-boundary";
        self.app
            .clone()
            .oneshot(authorized(
                Request::post("/api/v1/files")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header("Idempotency-Key", key)
                    .body(Body::from(single_file_body(
                        boundary, name, mime_type, bytes,
                    )))
                    .unwrap(),
            ))
            .await
            .unwrap()
    }

    async fn get(&self, path: &str) -> Response<Body> {
        self.app
            .clone()
            .oneshot(authorized(Request::get(path).body(Body::empty()).unwrap()))
            .await
            .unwrap()
    }

    async fn delete(&self, path: &str) -> Response<Body> {
        self.app
            .clone()
            .oneshot(authorized(
                Request::delete(path).body(Body::empty()).unwrap(),
            ))
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn upload_replay_conflict_read_and_delete_follow_the_contract() {
    let fixture = Fixture::new();
    let bytes = b"# Skill\n\nA durable file snapshot.";
    let created = fixture
        .upload(
            "upload-replay-key",
            "skill.md",
            "text/markdown; charset=utf-8",
            bytes,
        )
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert!(
        created["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("file_") && id.len() == 37)
    );
    assert_eq!(created["name"], "skill.md");
    assert_eq!(created["mimeType"], "text/markdown");
    assert_eq!(created["sizeBytes"], bytes.len());
    assert!(created["createdAt"].as_str().is_some());
    assert!(
        !created
            .to_string()
            .contains(&fixture.home.path().display().to_string())
    );

    let replay = fixture
        .upload(
            "upload-replay-key",
            "skill.md",
            "text/markdown; charset=utf-8",
            bytes,
        )
        .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(json_body(replay).await, created);

    let conflict = fixture
        .upload("upload-replay-key", "skill.md", "text/markdown", b"changed")
        .await;
    assert_problem(conflict, StatusCode::CONFLICT, "idempotency_conflict").await;

    let file_id = created["id"].as_str().unwrap();
    let content_path = format!("/api/v1/files/{file_id}/content");
    let content = fixture.get(&content_path).await;
    assert_eq!(content.status(), StatusCode::OK);
    assert_eq!(content.headers()[header::CONTENT_TYPE], "text/markdown");
    assert_eq!(content.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(content.headers()["x-content-type-options"], "nosniff");
    assert_eq!(body_bytes(content).await.as_ref(), bytes);

    let file_path = format!("/api/v1/files/{file_id}");
    assert_eq!(
        fixture.delete(&file_path).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_problem(
        fixture.get(&content_path).await,
        StatusCode::NOT_FOUND,
        "resource_not_found",
    )
    .await;
    assert_eq!(
        fixture.delete(&file_path).await.status(),
        StatusCode::NO_CONTENT
    );

    let deleted_replay = fixture
        .upload("upload-replay-key", "skill.md", "text/markdown", bytes)
        .await;
    assert_problem(
        deleted_replay,
        StatusCode::CONFLICT,
        "idempotency_resource_gone",
    )
    .await;
}

#[tokio::test]
async fn file_routes_are_authenticated_before_multipart_parsing() {
    let fixture = Fixture::new();
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/api/v1/files")
                .header(header::CONTENT_TYPE, "multipart/form-data")
                .body(Body::from("not multipart"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(response, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::get("/api/v1/files/file_00000000000000000000000000000000/content")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(response, StatusCode::UNAUTHORIZED, "unauthorized").await;
}

#[tokio::test]
async fn multipart_shape_filename_mime_id_and_size_boundaries_are_enforced() {
    let fixture = Fixture::new();

    let missing_boundary = fixture
        .app
        .clone()
        .oneshot(authorized(
            Request::post("/api/v1/files")
                .header(header::CONTENT_TYPE, "multipart/form-data")
                .header("Idempotency-Key", "missing-boundary-key")
                .body(Body::from("bad"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_problem(
        missing_boundary,
        StatusCode::BAD_REQUEST,
        "validation_failed",
    )
    .await;

    let boundary = "shape-boundary";
    let wrong_field = multipart_body(
        boundary,
        &[("metadata", Some("note.txt"), Some("text/plain"), b"x")],
    );
    let wrong_field = fixture
        .app
        .clone()
        .oneshot(authorized(
            Request::post("/api/v1/files")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("Idempotency-Key", "wrong-field-key")
                .body(Body::from(wrong_field))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_problem(wrong_field, StatusCode::BAD_REQUEST, "validation_failed").await;

    let extra_field = multipart_body(
        boundary,
        &[
            ("file", Some("note.txt"), Some("text/plain"), b"x"),
            ("other", None, None, b"unexpected"),
        ],
    );
    let extra_field = fixture
        .app
        .clone()
        .oneshot(authorized(
            Request::post("/api/v1/files")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("Idempotency-Key", "extra-field-key")
                .body(Body::from(extra_field))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_problem(extra_field, StatusCode::BAD_REQUEST, "validation_failed").await;

    assert_problem(
        fixture
            .upload("unsafe-name-key", "../outside.txt", "text/plain", b"x")
            .await,
        StatusCode::BAD_REQUEST,
        "validation_failed",
    )
    .await;
    assert_problem(
        fixture
            .upload("unsupported-mime-key", "page.html", "text/html", b"x")
            .await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_file_type",
    )
    .await;

    assert_problem(
        fixture.get("/api/v1/files/file_bad/content").await,
        StatusCode::BAD_REQUEST,
        "validation_failed",
    )
    .await;
    assert_problem(
        fixture
            .get("/api/v1/files/file_00000000000000000000000000000000/content")
            .await,
        StatusCode::NOT_FOUND,
        "resource_not_found",
    )
    .await;

    let oversized = vec![0x5a; MAX_FILE_BYTES + 1];
    assert_problem(
        fixture
            .upload(
                "content-too-large-key",
                "large.bin",
                "application/octet-stream",
                &oversized,
            )
            .await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    )
    .await;
}

#[tokio::test]
async fn upload_route_overrides_the_smaller_default_json_body_limit() {
    let fixture = Fixture::new();
    let bytes = vec![0x41; MAX_FILE_BYTES];
    let response = fixture
        .upload("exact-file-limit-key", "maximum.txt", "text/plain", &bytes)
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let file = json_body(response).await;
    assert_eq!(file["sizeBytes"], bytes.len());

    let content = fixture
        .get(&format!(
            "/api/v1/files/{}/content",
            file["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(content.status(), StatusCode::OK);
    assert_eq!(body_bytes(content).await.len(), bytes.len());
}

#[tokio::test]
async fn retained_byte_quota_returns_static_507_and_delete_releases_capacity() {
    let fixture = Fixture::new();
    let object_count = MAX_RETAINED_FILE_BYTES / MAX_FILE_BYTES as u64;
    assert_eq!(
        object_count * MAX_FILE_BYTES as u64,
        MAX_RETAINED_FILE_BYTES
    );
    let zeroes = vec![0_u8; MAX_FILE_BYTES];
    let sha256 = hex_digest(&Sha256::digest(&zeroes));
    let mut first_file_id = None;
    for index in 0..object_count {
        let file_id = seed_file_object(fixture.home.path(), index, MAX_FILE_BYTES as u64, &sha256);
        first_file_id.get_or_insert(file_id);
    }

    let rejected = fixture
        .upload("quota-http-rejected", "extra.txt", "text/plain", b"x")
        .await;
    assert_eq!(rejected.status(), StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(
        rejected.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let problem = json_body(rejected).await;
    assert_eq!(problem["status"], StatusCode::INSUFFICIENT_STORAGE.as_u16());
    assert_eq!(problem["code"], "file_quota_exceeded");
    assert_eq!(
        problem["detail"],
        "The retained file store cannot accept another snapshot."
    );
    let serialized = problem.to_string();
    assert!(!serialized.contains(&fixture.home.path().display().to_string()));
    assert!(!serialized.contains(&MAX_RETAINED_FILE_BYTES.to_string()));
    assert!(problem.get("usedBytes").is_none());
    assert!(problem.get("retainedBytes").is_none());
    assert!(problem.get("retainedObjects").is_none());

    let first_file_id = first_file_id.unwrap();
    assert_eq!(
        fixture
            .delete(&format!("/api/v1/files/{first_file_id}"))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .upload("quota-http-released", "extra.txt", "text/plain", b"x")
            .await
            .status(),
        StatusCode::CREATED
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symbolic_link_object_directory_is_rejected_without_following_it() {
    use std::{fs, os::unix::fs::symlink};

    let fixture = Fixture::new();
    let created = json_body(
        fixture
            .upload("symlink-object-key", "note.txt", "text/plain", b"inside")
            .await,
    )
    .await;
    let file_id = created["id"].as_str().unwrap();
    let objects = fixture.home.path().join(".synthchat/files/objects");
    let object = objects.join(file_id);
    let backup = objects.join("object-backup");
    fs::rename(&object, &backup).unwrap();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), &object).unwrap();

    let response = fixture
        .get(&format!("/api/v1/files/{file_id}/content"))
        .await;
    assert_problem(response, StatusCode::CONFLICT, "unsafe_file_path").await;

    fs::remove_file(&object).unwrap();
    fs::rename(backup, object).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn reparse_point_object_directory_is_rejected_without_following_it() {
    use std::{fs, io, os::windows::fs::symlink_dir};

    let fixture = Fixture::new();
    let created = json_body(
        fixture
            .upload("reparse-object-key", "note.txt", "text/plain", b"inside")
            .await,
    )
    .await;
    let file_id = created["id"].as_str().unwrap();
    let objects = fixture.home.path().join(".synthchat/files/objects");
    let object = objects.join(file_id);
    let backup = objects.join("object-backup");
    fs::rename(&object, &backup).unwrap();
    let outside = TempDir::new().unwrap();
    match symlink_dir(outside.path(), &object) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            fs::rename(backup, object).unwrap();
            return;
        }
        Err(error) => panic!("failed to create the reparse-point test fixture: {error}"),
    }

    let response = fixture
        .get(&format!("/api/v1/files/{file_id}/content"))
        .await;
    assert_problem(response, StatusCode::CONFLICT, "unsafe_file_path").await;

    fs::remove_dir(&object).unwrap();
    fs::rename(backup, object).unwrap();
}

#[tokio::test]
async fn capabilities_publish_the_effective_file_limits_and_mime_allowlist() {
    let fixture = Fixture::new();
    let capabilities = json_body(fixture.get("/api/v1/capabilities").await).await;
    assert_eq!(capabilities["files"]["maxBytes"], MAX_FILE_BYTES);
    let allowed = capabilities["files"]["allowedMimeTypes"]
        .as_array()
        .unwrap();
    assert!(allowed.contains(&json!("application/zip")));
    assert!(allowed.contains(&json!("image/png")));
    assert!(allowed.contains(&json!("text/markdown")));
    assert!(!allowed.contains(&json!("text/html")));
}

fn authorized(mut request: Request<Body>) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    request
}

fn single_file_body(boundary: &str, name: &str, mime_type: &str, bytes: &[u8]) -> Vec<u8> {
    multipart_body(boundary, &[("file", Some(name), Some(mime_type), bytes)])
}

fn multipart_body(boundary: &str, parts: &[MultipartPart<'_>]) -> Vec<u8> {
    let mut body = Vec::new();
    for (field_name, file_name, mime_type, bytes) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{field_name}\"").as_bytes(),
        );
        if let Some(file_name) = file_name {
            body.extend_from_slice(format!("; filename=\"{file_name}\"").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        if let Some(mime_type) = mime_type {
            body.extend_from_slice(format!("Content-Type: {mime_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn seed_file_object(home: &std::path::Path, index: u64, size_bytes: u64, sha256: &str) -> String {
    let file_id = format!("file_{index:032x}");
    let object = home.join(".synthchat/files/objects").join(&file_id);
    std::fs::create_dir_all(&object).unwrap();
    std::fs::File::create(object.join("content"))
        .unwrap()
        .set_len(size_bytes)
        .unwrap();
    std::fs::write(
        object.join("metadata.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "file": {
                "id": &file_id,
                "name": format!("seed-{index}.bin"),
                "mimeType": "application/octet-stream",
                "sizeBytes": size_bytes,
                "createdAt": "2026-01-01T00:00:00Z"
            },
            "sha256": sha256
        }))
        .unwrap(),
    )
    .unwrap();
    file_id
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
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
    assert!(body.get("requestId").is_some());
    let serialized = body.to_string();
    assert!(!serialized.contains(".synthchat"));
    assert!(!serialized.contains("\\\\"));
}

async fn json_body(response: Response<Body>) -> Value {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

async fn body_bytes(response: Response<Body>) -> axum::body::Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}
