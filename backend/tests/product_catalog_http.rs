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

#[tokio::test]
async fn persona_crud_uses_strong_product_etags() {
    let (app, _home) = app();
    let path = "/api/v1/profiles/default/personas";
    let created = request(
        &app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "name": "小可",
                    "characterPrompt": "温柔可靠",
                    "model": "gpt-test",
                    "temperature": 0.8,
                    "maxTokens": 2048
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[header::ETAG], "\"product-persona-1\"");
    let created_body = body(created).await;
    let id = created_body["id"].as_str().unwrap().to_owned();
    assert_eq!(created_body["name"], "小可");
    assert_eq!(created_body["model"], "gpt-test");

    let fetched = request(
        &app,
        Request::get(format!("{path}/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(fetched.headers()[header::ETAG], "\"product-persona-1\"");
    assert_eq!(body(fetched).await["id"], id);

    let updated = request(
        &app,
        Request::patch(format!("{path}/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, "\"product-persona-1\"")
            .body(Body::from(json!({"name": "小可 2"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(updated.headers()[header::ETAG], "\"product-persona-2\"");
    assert_eq!(body(updated).await["name"], "小可 2");

    let stale = request(
        &app,
        Request::patch(format!("{path}/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, "\"product-persona-1\"")
            .body(Body::from(json!({"name": "过期"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_problem(stale, StatusCode::CONFLICT, "revision_conflict").await;

    let listed = request(&app, Request::get(path).body(Body::empty()).unwrap()).await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(body(listed).await.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn worldbook_and_moments_support_nested_mutations() {
    let (app, _home) = app();
    let worldbook = request(
        &app,
        Request::post("/api/v1/profiles/default/worldbooks")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "name": "城市设定",
                    "description": "海边城市",
                    "sections": [{"key": "地点", "content": "旧港", "enabled": true}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(worldbook.status(), StatusCode::CREATED);
    assert_eq!(worldbook.headers()[header::ETAG], "\"product-worldbook-1\"");
    let worldbook = body(worldbook).await;
    let worldbook_id = worldbook["id"].as_str().unwrap().to_owned();
    assert_eq!(worldbook["sections"][0]["key"], "地点");
    let fetched_worldbook = request(
        &app,
        Request::get(format!(
            "/api/v1/profiles/default/worldbooks/{worldbook_id}"
        ))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(fetched_worldbook.status(), StatusCode::OK);
    assert_eq!(
        fetched_worldbook.headers()[header::ETAG],
        "\"product-worldbook-1\""
    );

    let moment = request(
        &app,
        Request::post("/api/v1/profiles/default/moments")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"authorId": "user", "body": "今天很开心"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(moment.status(), StatusCode::CREATED);
    assert_eq!(moment.headers()[header::ETAG], "\"product-moment-1\"");
    let moment = body(moment).await;
    let id = moment["id"].as_str().unwrap().to_owned();
    let fetched_moment = request(
        &app,
        Request::get(format!("/api/v1/profiles/default/moments/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched_moment.status(), StatusCode::OK);
    assert_eq!(
        fetched_moment.headers()[header::ETAG],
        "\"product-moment-1\""
    );

    let commented = request(
        &app,
        Request::post(format!("/api/v1/profiles/default/moments/{id}/comments"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, "\"product-moment-1\"")
            .body(Body::from(
                json!({"authorId": "小可", "text": "真好"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(commented.status(), StatusCode::OK);
    let commented = body(commented).await;
    assert_eq!(commented["comments"].as_array().unwrap().len(), 1);
    assert_eq!(commented["revision"], 2);

    let liked = request(
        &app,
        Request::put(format!("/api/v1/profiles/default/moments/{id}/like"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, "\"product-moment-2\"")
            .body(Body::from(
                json!({"actorId": "user", "liked": true}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(liked.status(), StatusCode::OK);
    assert_eq!(body(liked).await["likedBy"], json!(["user"]));
}

#[tokio::test]
async fn product_records_are_profile_scoped() {
    let (app, _home) = app();
    let created = request(
        &app,
        Request::post("/api/v1/profiles/default/personas")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"name": "默认"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let other = request(
        &app,
        Request::get("/api/v1/profiles/other/personas")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
}

async fn request(app: &Router, mut request: Request<Body>) -> Response<Body> {
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
