use std::{ffi::OsString, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use synthchat_hermes_backend::{AppConfig, ProfileService, build_router};
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const DISCOVERY_ENVIRONMENT: [&str; 4] = [
    "SYNTHCHAT_CODE_EXECUTION_PYTHON",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "PATH",
];

struct EnvironmentGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvironmentGuard {
    fn clear(names: &'static [&'static str]) -> Self {
        let original = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        for name in names {
            // This integration-test binary contains one test, so no sibling test
            // can read the process environment while discovery is constrained.
            unsafe { std::env::remove_var(name) };
        }
        Self(original)
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..) {
            // See EnvironmentGuard::clear: restoration runs in the same
            // single-test process after all requests have completed.
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
}

#[tokio::test]
async fn missing_python_disables_capability_and_toolset_configuration() {
    let _environment = EnvironmentGuard::clear(&DISCOVERY_ENVIRONMENT);
    let home = tempfile::tempdir().unwrap();
    let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
    let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
    let app = build_router(AppConfig::new(
        TOKEN.to_owned(),
        vec!["tauri://localhost".parse().unwrap()],
        profiles,
    ));

    let capabilities = get_json(app.clone(), "/api/v1/capabilities").await;
    assert_eq!(capabilities["engine"]["available"], true);
    assert_eq!(capabilities["sessionStorage"]["available"], true);
    assert_eq!(capabilities["extensions"]["codeExecution"], false);

    let toolsets = get_json(app, "/api/v1/profiles/default/toolsets").await;
    let code_execution = toolsets
        .as_array()
        .unwrap()
        .iter()
        .find(|toolset| toolset["id"] == "code_execution")
        .unwrap();
    assert_eq!(code_execution["configured"], false);
}

async fn get_json(app: axum::Router, path: &str) -> serde_json::Value {
    let response = app
        .oneshot(
            Request::get(path)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
