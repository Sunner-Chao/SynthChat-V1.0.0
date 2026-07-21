use std::{collections::BTreeMap, sync::Arc};

mod error;
mod file_routes;
mod import_routes;
mod mcp_routes;
mod memory_routes;
mod operation_routes;
mod plugin_routes;
mod product_routes;
mod profile_routes;
mod run_routes;
mod session_routes;
mod skill_routes;
mod tool_routes;
mod web_routes;
mod wechat_routes;
mod workspace_routes;

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, OriginalUri, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method,
        header::{AUTHORIZATION, CONTENT_TYPE, ETAG, WWW_AUTHENTICATE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use subtle::ConstantTimeEq;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    files::FileService,
    mcp::McpService,
    memory::MemoryService,
    plugins::PluginService,
    product_catalog::ProductCatalogService,
    profiles::ProfileService,
    runs::RunService,
    sessions::SessionService,
    skills::{SkillRegistryRuntimeConfig, SkillService},
    web::{WebRuntimeConfig, WebService},
    wechat::WechatService,
};

use self::error::{ApiError, RequestContext};

static REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

pub struct AppConfig {
    desktop_token: String,
    allowed_origins: Vec<HeaderValue>,
    profiles: ProfileService,
    sessions: SessionService,
    skill_registry_runtime_config: SkillRegistryRuntimeConfig,
    web_runtime_config: WebRuntimeConfig,
    #[cfg(debug_assertions)]
    web_base_url: Option<url::Url>,
}

impl AppConfig {
    pub fn new(
        desktop_token: String,
        allowed_origins: Vec<HeaderValue>,
        profiles: ProfileService,
    ) -> Self {
        let sessions = SessionService::new(profiles.hermes_home(), &desktop_token);
        Self {
            desktop_token,
            allowed_origins,
            profiles,
            sessions,
            skill_registry_runtime_config: SkillRegistryRuntimeConfig::default(),
            web_runtime_config: WebRuntimeConfig::default(),
            #[cfg(debug_assertions)]
            web_base_url: None,
        }
    }

    pub fn with_web_runtime_config(mut self, config: WebRuntimeConfig) -> Self {
        self.web_runtime_config = config;
        self
    }

    pub(crate) fn with_skill_registry_runtime_config(
        mut self,
        config: SkillRegistryRuntimeConfig,
    ) -> Self {
        self.skill_registry_runtime_config = config;
        self
    }

    #[cfg(debug_assertions)]
    pub fn with_web_base_url_for_tests(mut self, base_url: url::Url) -> Self {
        self.web_base_url = Some(base_url);
        self
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    desktop_token: Arc<str>,
    profiles: Arc<ProfileService>,
    sessions: Arc<SessionService>,
    files: Arc<FileService>,
    runs: Arc<RunService>,
    skills: Arc<SkillService>,
    memory: Arc<MemoryService>,
    mcp: Arc<McpService>,
    web: Arc<WebService>,
    wechat: Arc<WechatService>,
    plugins: Arc<PluginService>,
    product_catalog: Arc<ProductCatalogService>,
}

pub struct AppShutdown {
    runs: Arc<RunService>,
}

impl AppShutdown {
    pub async fn shutdown(self) {
        self.runs.shutdown().await;
    }

    pub async fn shutdown_preserving_runs(self) {
        self.runs.shutdown_preserving_runs().await;
    }
}

#[derive(Clone)]
pub(crate) struct RequestId(pub(crate) String);

pub fn build_router(config: AppConfig) -> Router {
    build_router_with_shutdown(config).0
}

pub fn build_router_with_shutdown(config: AppConfig) -> (Router, AppShutdown) {
    let profiles = Arc::new(config.profiles);
    let sessions = Arc::new(config.sessions);
    let files = Arc::new(FileService::new(profiles.hermes_home()));
    let skills = Arc::new(SkillService::with_file_service(
        profiles.clone(),
        files.clone(),
        &config.desktop_token,
        config.skill_registry_runtime_config,
    ));
    let memory = Arc::new(MemoryService::new(profiles.clone(), &config.desktop_token));
    let mcp = Arc::new(McpService::new(profiles.clone()));
    let plugins = Arc::new(PluginService::new(profiles.hermes_home()));
    let product_catalog = Arc::new(ProductCatalogService::new(profiles.hermes_home()));
    let wechat = Arc::new(WechatService::new(
        profiles.clone(),
        product_catalog.clone(),
    ));
    let web_runtime_config = config.web_runtime_config.clone();
    let web = Arc::new({
        #[cfg(debug_assertions)]
        let service = if let Some(base_url) = config.web_base_url.as_ref() {
            WebService::with_base_url(profiles.clone(), base_url.as_str())
        } else {
            WebService::with_runtime_config(profiles.clone(), web_runtime_config.clone())
        };
        #[cfg(not(debug_assertions))]
        let service = WebService::with_runtime_config(profiles.clone(), web_runtime_config.clone());

        service.unwrap_or_else(|error| {
            tracing::error!(
                ?error,
                "failed to initialize Web service; Web execution disabled"
            );
            WebService::unavailable_with_runtime_config(
                profiles.clone(),
                web_runtime_config.clone(),
            )
        })
    });
    let runs = Arc::new(RunService::new(
        profiles.clone(),
        sessions.clone(),
        skills.clone(),
        memory.clone(),
        web.clone(),
    ));
    let shutdown = AppShutdown { runs: runs.clone() };
    let state = AppState {
        desktop_token: Arc::from(config.desktop_token),
        profiles,
        sessions,
        files,
        runs,
        skills,
        memory,
        mcp,
        web,
        wechat,
        plugins,
        product_catalog,
    };
    let protected = Router::new()
        .route("/api/v1/capabilities", get(get_capabilities))
        .merge(file_routes::routes())
        .merge(operation_routes::routes())
        .merge(profile_routes::routes())
        .merge(session_routes::routes())
        .merge(import_routes::routes())
        .merge(memory_routes::routes())
        .merge(mcp_routes::routes())
        .merge(run_routes::routes())
        .merge(skill_routes::routes())
        .merge(tool_routes::routes())
        .merge(web_routes::routes())
        .merge(wechat_routes::routes())
        .merge(plugin_routes::routes())
        .merge(product_routes::routes())
        .merge(workspace_routes::routes())
        .fallback(protected_not_found)
        .method_not_allowed_fallback(protected_method_not_allowed)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    let router = Router::new()
        .route("/health", get(get_health))
        .merge(protected)
        .with_state(state)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer(config.allowed_origins))
        .layer(middleware::from_fn(assign_request_id));
    (router, shutdown)
}

fn cors_layer(allowed_origins: Vec<HeaderValue>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            AUTHORIZATION,
            CONTENT_TYPE,
            HeaderName::from_static("idempotency-key"),
            HeaderName::from_static("if-match"),
            HeaderName::from_static("last-event-id"),
            REQUEST_ID_HEADER.clone(),
        ])
        .expose_headers([ETAG, REQUEST_ID_HEADER.clone()])
}

async fn get_health() -> Json<Health> {
    Json(Health {
        status: "ok",
        service: "synthchat-hermes-backend",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn get_capabilities(State(state): State<AppState>) -> Json<Capabilities> {
    Json(Capabilities::current(
        &state.sessions,
        &state.runs,
        &state.web,
        &state.files,
        &state.skills,
        &state.mcp,
    ))
}

async fn require_bearer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if authorized(request.headers(), &state.desktop_token) {
        return next.run(request).await;
    }

    let instance = request.uri().path().to_owned();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let context = RequestContext::new(RequestId(request_id), instance);
    let mut response = ApiError::unauthorized(context).into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"synthchat-desktop\""),
    );
    response
}

async fn protected_not_found(
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
) -> ApiError {
    ApiError::not_found(RequestContext::new(request_id, uri.path()))
}

async fn protected_method_not_allowed(
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
) -> ApiError {
    ApiError::method_not_allowed(RequestContext::new(request_id, uri.path()))
}

fn authorized(headers: &HeaderMap, expected_token: &str) -> bool {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(header) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }

    let Ok(value) = header.to_str() else {
        return false;
    };
    let Some((scheme, provided_token)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("bearer") || provided_token.is_empty() {
        return false;
    }

    bool::from(provided_token.as_bytes().ct_eq(expected_token.as_bytes()))
}

async fn assign_request_id(mut request: Request<Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| crate::operations::valid_origin_request_id(value))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let header_value = HeaderValue::from_str(&request_id)
        .expect("a validated or generated request ID is a valid header value");

    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER.clone(), header_value.clone());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER.clone(), header_value);
    response
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    contract_version: &'static str,
    backend_version: &'static str,
    engine: EngineCapabilities,
    session_storage: SessionStorageCapabilities,
    session_search: SessionSearchCapabilities,
    files: FileCapabilities,
    extensions: BTreeMap<String, serde_json::Value>,
}

impl Capabilities {
    fn current(
        sessions: &SessionService,
        runs: &RunService,
        web: &WebService,
        files: &FileService,
        skills: &SkillService,
        mcp: &McpService,
    ) -> Self {
        let run_streaming = runs.is_available();
        let web_available = web.is_available();
        let browser_available = runs.browser_available();
        let browser_downloads_available = runs.browser_downloads_available();
        let extensions = BTreeMap::from([
            ("activeRunDiscovery".to_owned(), serde_json::json!(true)),
            ("runQueue".to_owned(), serde_json::json!(run_streaming)),
            ("toolExecution".to_owned(), serde_json::json!(true)),
            (
                "codeExecution".to_owned(),
                serde_json::json!(runs.code_execution_available()),
            ),
            ("toolsetManagement".to_owned(), serde_json::json!(true)),
            ("skillDiscovery".to_owned(), serde_json::json!(true)),
            ("skillEnablement".to_owned(), serde_json::json!(true)),
            ("workspaceManagement".to_owned(), serde_json::json!(true)),
            ("webSearch".to_owned(), serde_json::json!(web_available)),
            ("webExtract".to_owned(), serde_json::json!(web_available)),
            (
                "browserAutomation".to_owned(),
                serde_json::json!(browser_available),
            ),
            (
                "browserCdp".to_owned(),
                serde_json::json!(browser_available),
            ),
            (
                "browserDownloads".to_owned(),
                serde_json::json!(browser_downloads_available),
            ),
            (
                "mcpStdio".to_owned(),
                serde_json::json!(mcp.runtime_available()),
            ),
            (
                "mcpStreamableHttp".to_owned(),
                serde_json::json!(mcp.streamable_http_available()),
            ),
            ("mcpSse".to_owned(), serde_json::json!(mcp.sse_available())),
            ("wechatAccounts".to_owned(), serde_json::json!(true)),
            ("wechatMessaging".to_owned(), serde_json::json!(true)),
            ("plugins".to_owned(), serde_json::json!(true)),
            ("personas".to_owned(), serde_json::json!(true)),
            ("moments".to_owned(), serde_json::json!(true)),
            ("worldbooks".to_owned(), serde_json::json!(true)),
        ]);
        Self {
            contract_version: "v1",
            backend_version: env!("CARGO_PKG_VERSION"),
            engine: EngineCapabilities {
                kind: if run_streaming {
                    "hermes-rust"
                } else {
                    "unavailable"
                },
                available: run_streaming,
                version: run_streaming.then_some(env!("CARGO_PKG_VERSION")),
                pinned_commit: run_streaming.then_some("3f2a389c7e1f1729cad91ae63c26fb08c7753c74"),
                features: EngineFeatures {
                    run_streaming,
                    reasoning_streaming: run_streaming,
                    tool_progress: run_streaming,
                    approvals: run_streaming,
                    clarifications: run_streaming,
                    async_tool_delivery: run_streaming,
                    profile_management: true,
                    skill_management: skills.management_available(),
                    memory_write: run_streaming,
                    mcp_management: mcp.runtime_available(),
                    ..EngineFeatures::default()
                },
            },
            session_storage: SessionStorageCapabilities {
                available: sessions.is_available(),
                schema_version: sessions.schema_version(),
                hermes_import_available: sessions.is_available(),
            },
            session_search: SessionSearchCapabilities {
                mode: sessions.search_mode().as_str(),
            },
            files: FileCapabilities {
                max_bytes: files.max_bytes(),
                allowed_mime_types: files.allowed_mime_types().to_vec(),
            },
            extensions,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineCapabilities {
    kind: &'static str,
    available: bool,
    version: Option<&'static str>,
    pinned_commit: Option<&'static str>,
    features: EngineFeatures,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineFeatures {
    run_streaming: bool,
    reasoning_streaming: bool,
    tool_progress: bool,
    approvals: bool,
    clarifications: bool,
    async_tool_delivery: bool,
    profile_management: bool,
    skill_management: bool,
    memory_write: bool,
    mcp_management: bool,
    oauth_accounts: bool,
}

#[derive(Serialize)]
struct SessionSearchCapabilities {
    mode: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionStorageCapabilities {
    available: bool,
    schema_version: Option<u32>,
    hermes_import_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileCapabilities {
    max_bytes: u64,
    allowed_mime_types: Vec<&'static str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_scheme_is_case_insensitive_and_duplicate_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("bearer expected"));
        assert!(authorized(&headers, "expected"));

        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer expected"));
        assert!(!authorized(&headers, "expected"));
    }

    #[test]
    fn malformed_or_wrong_credentials_are_rejected() {
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, "expected"));

        headers.insert(AUTHORIZATION, HeaderValue::from_static("Basic expected"));
        assert!(!authorized(&headers, "expected"));

        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(!authorized(&headers, "expected"));
    }
}
