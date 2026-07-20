use std::{
    collections::HashMap,
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, LazyLock, Mutex, OnceLock},
    time::Duration,
};

mod runtime_config;

pub use runtime_config::{TAVILY_BASE_URL_ENV, WebRuntimeConfig, WebRuntimeConfigError};

use crate::{
    profiles::{ProfileError, ProfileService, WebProvider, WebProviderStatus},
    tools::{ToolExecutionControl, ToolExecutionControlError},
};
use futures_util::StreamExt;
use regex::Regex;
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE},
    redirect::Policy,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;
use tokio::{
    net::lookup_host,
    sync::{OnceCell, OwnedSemaphorePermit, Semaphore, TryAcquireError},
};
use url::{Host, Url};

const TAVILY_API_KEY: &str = "TAVILY_API_KEY";
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROVIDER_CONTENT_BYTES: usize = 100_000;
// Leave room for keys and array/object framing after charging every retained
// string for its exact serde_json escaping cost.
const MAX_NORMALIZED_STRING_COST: usize = 94_000;
const GLOBAL_CONCURRENCY: usize = 8;
const PROFILE_CONCURRENCY: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(20);
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(20);
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

static GLOBAL_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

static KNOWN_CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        (?:bearer\s+[a-z0-9._~+/=-]{8,})
        |(?:-----begin\s+(?:rsa\s+|ec\s+|openssh\s+)?private\s+key-----)
        |(?:AKIA[0-9A-Z]{16})
        |(?:AIza[0-9A-Za-z_-]{20,})
        |(?:(?:sk|tvly|gh[pousr]|github_pat|xox[baprs])[-_][0-9A-Za-z._-]{8,})
        |(?:eyJ[0-9A-Za-z_-]{8,}\.[0-9A-Za-z_-]{8,}\.[0-9A-Za-z_-]{8,})
        |(?:(?:api[-_ ]?key|access[-_ ]?token|secret|password|passwd)\s*[:=]\s*[^\s,;]{4,})",
    )
    .expect("credential pattern is valid")
});

static ANSI_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")
        .expect("ANSI pattern is valid")
});

static INLINE_DATA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:data|blob):[^\s\"']+|base64,[0-9a-z+/=]{32,}|[0-9a-z+/]{256,}={0,2}"#)
        .expect("inline data pattern is valid")
});

#[derive(Clone)]
pub(crate) struct WebService {
    inner: Arc<WebServiceInner>,
}

struct WebServiceInner {
    profiles: Arc<ProfileService>,
    client: ProviderClient,
    base_url: Url,
    profile_gates: Mutex<HashMap<String, Arc<Semaphore>>>,
}

enum ProviderClient {
    Runtime(OnceCell<Client>),
    #[cfg(any(test, debug_assertions))]
    Test(Client),
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebReadiness {
    pub(crate) search_ready: bool,
    pub(crate) extract_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebExecutionOutput {
    pub(crate) raw_result_json: String,
    pub(crate) provider_content: String,
    pub(crate) input_summary: String,
    pub(crate) result_summary: String,
}

#[derive(Error)]
pub(crate) enum WebError {
    #[error("web execution is unavailable")]
    Unavailable,
    #[error("web tool arguments are invalid")]
    InvalidArguments,
    #[error("web input violates the network safety policy")]
    UnsafeInput,
    #[error("the configured web provider secret is unavailable")]
    MissingSecret,
    #[error("web execution capacity is unavailable")]
    Busy,
    #[error("web provider transport failed")]
    Transport,
    #[error("web provider returned an invalid response")]
    InvalidResponse,
    #[error("web execution was cancelled")]
    Cancelled,
    #[error("web execution deadline was exceeded")]
    DeadlineExceeded,
    #[error("profile operation failed")]
    Profile,
}

impl fmt::Debug for WebError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Profile/storage and transport internals can contain host paths or
        // endpoint details. Debug output intentionally shares static Display.
        fmt::Display::fmt(self, formatter)
    }
}

impl From<ProfileError> for WebError {
    fn from(_error: ProfileError) -> Self {
        Self::Profile
    }
}

impl WebService {
    #[cfg(test)]
    pub(crate) fn new(profiles: Arc<ProfileService>) -> Result<Self, WebError> {
        Self::with_runtime_config(profiles, WebRuntimeConfig::default())
    }

    pub(crate) fn with_runtime_config(
        profiles: Arc<ProfileService>,
        config: WebRuntimeConfig,
    ) -> Result<Self, WebError> {
        Ok(Self {
            inner: Arc::new(WebServiceInner {
                profiles,
                client: ProviderClient::Runtime(OnceCell::new()),
                base_url: config.tavily_base_url().clone(),
                profile_gates: Mutex::new(HashMap::new()),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn unavailable(profiles: Arc<ProfileService>) -> Self {
        Self::unavailable_with_runtime_config(profiles, WebRuntimeConfig::default())
    }

    pub(crate) fn unavailable_with_runtime_config(
        profiles: Arc<ProfileService>,
        config: WebRuntimeConfig,
    ) -> Self {
        Self {
            inner: Arc::new(WebServiceInner {
                profiles,
                client: ProviderClient::Unavailable,
                base_url: config.tavily_base_url().clone(),
                profile_gates: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub(crate) fn with_base_url(
        profiles: Arc<ProfileService>,
        base_url: impl AsRef<str>,
    ) -> Result<Self, WebError> {
        let base_url = normalized_test_provider_base_url(base_url.as_ref())?;
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_HTTP_TIMEOUT)
            .user_agent("SynthChat-Hermes-Rust/0.1 web-tavily")
            .build()
            .map_err(|_| WebError::Unavailable)?;
        Ok(Self {
            inner: Arc::new(WebServiceInner {
                profiles,
                client: ProviderClient::Test(client),
                base_url,
                profile_gates: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub(crate) fn is_available(&self) -> bool {
        !matches!(self.inner.client, ProviderClient::Unavailable)
    }

    pub(crate) fn providers(&self) -> Vec<WebProvider> {
        vec![WebProvider {
            id: "tavily".to_owned(),
            display_name: "Tavily".to_owned(),
            supports_search: true,
            supports_extract: true,
            secret_names: vec![TAVILY_API_KEY.to_owned()],
            default_base_url: self
                .inner
                .base_url
                .as_str()
                .trim_end_matches('/')
                .to_owned(),
            custom_endpoint_supported: false,
        }]
    }

    pub(crate) fn readiness(&self, profile_id: &str) -> Result<WebReadiness, WebError> {
        if !self.is_available() {
            return Ok(WebReadiness::default());
        }
        let config = self.inner.profiles.get_web_config(profile_id)?.value;
        Ok(WebReadiness {
            search_ready: effective_tavily_ready(
                config.effective_search.provider_id.as_deref(),
                config.effective_search.status,
            ),
            extract_ready: effective_tavily_ready(
                config.effective_extract.provider_id.as_deref(),
                config.effective_extract.status,
            ),
        })
    }

    pub(crate) async fn execute_search(
        &self,
        profile_id: &str,
        raw_arguments_json: &str,
        control: ToolExecutionControl,
    ) -> Result<WebExecutionOutput, WebError> {
        check_control(&control)?;
        let input: SearchInput = strict_arguments(raw_arguments_json)?;
        validate_query(&input.query)?;
        let limit = input.limit.unwrap_or(5);
        if !(1..=100).contains(&limit) {
            return Err(WebError::InvalidArguments);
        }

        let config = self.inner.profiles.get_web_config(profile_id)?.value;
        require_effective_tavily(
            config.effective_search.provider_id.as_deref(),
            config.effective_search.status,
        )?;
        let call_secrets = self.load_call_secrets(profile_id)?;
        scan_sensitive(&input.query, &call_secrets.redaction)?;
        let _permits = self.acquire_permits(profile_id, &control).await?;

        let body = json!({
            "query": input.query,
            "max_results": limit.min(20),
            "include_raw_content": false,
            "include_images": false,
        });
        let response = self
            .post_json("search", &body, &call_secrets.api_key, &control)
            .await?;
        let response: TavilySearchResponse =
            serde_json::from_slice(&response).map_err(|_| WebError::InvalidResponse)?;
        let results = response.results;
        if results.len() > 100 {
            return Err(WebError::InvalidResponse);
        }

        let normalized = self
            .normalize_search_results(
                results.into_iter().take(limit.min(20)),
                &call_secrets.redaction,
                &control,
            )
            .await?;
        let result_count = normalized.len();
        build_output(
            json!({
                "externalUntrusted": true,
                "success": true,
                "data": { "web": normalized },
            }),
            "Web search requested".to_owned(),
            format!("Found {result_count} web result(s)"),
        )
    }

    pub(crate) async fn execute_extract(
        &self,
        profile_id: &str,
        raw_arguments_json: &str,
        control: ToolExecutionControl,
    ) -> Result<WebExecutionOutput, WebError> {
        check_control(&control)?;
        let input: ExtractInput = strict_arguments(raw_arguments_json)?;
        if input.urls.is_empty() || input.urls.len() > 5 {
            return Err(WebError::InvalidArguments);
        }

        let config = self.inner.profiles.get_web_config(profile_id)?.value;
        require_effective_tavily(
            config.effective_extract.provider_id.as_deref(),
            config.effective_extract.status,
        )?;
        let char_limit = input.char_limit.unwrap_or(config.extract_char_limit);
        if !(2_000..=500_000).contains(&char_limit) {
            return Err(WebError::InvalidArguments);
        }

        let call_secrets = self.load_call_secrets(profile_id)?;
        for raw_url in &input.urls {
            if raw_url.is_empty() || raw_url.len() > 8_192 {
                return Err(WebError::InvalidArguments);
            }
            scan_sensitive(raw_url, &call_secrets.redaction)?;
        }
        let _permits = self.acquire_permits(profile_id, &control).await?;

        let mut slots = Vec::with_capacity(input.urls.len());
        let mut urls = Vec::with_capacity(input.urls.len());
        for raw_url in input.urls {
            match self
                .validate_public_url(&raw_url, &call_secrets.redaction, &control)
                .await
            {
                Ok(url) => {
                    urls.push(url.clone());
                    slots.push(ExtractInputSlot::Safe(url));
                }
                Err(WebError::UnsafeInput) => slots.push(ExtractInputSlot::Blocked(raw_url)),
                Err(error) => return Err(error),
            }
        }
        let response = if urls.is_empty() {
            TavilyExtractResponse::default()
        } else {
            let body = json!({
                "urls": urls,
                "include_images": false,
            });
            let response = self
                .post_json("extract", &body, &call_secrets.api_key, &control)
                .await?;
            serde_json::from_slice(&response).map_err(|_| WebError::InvalidResponse)?
        };
        if response.results.len() > 5
            || response.failed_results.len() > 5
            || response.failed_urls.len() > 5
        {
            return Err(WebError::InvalidResponse);
        }

        let normalized = self
            .normalize_extract_results(
                &slots,
                response,
                char_limit,
                &call_secrets.redaction,
                &control,
            )
            .await?;
        let result_count = normalized
            .iter()
            .filter(|result| result.error.is_none())
            .count();
        build_output(
            json!({
                "externalUntrusted": true,
                "results": normalized,
            }),
            "Web extraction requested".to_owned(),
            format!("Extracted {result_count} web page(s)"),
        )
    }

    fn load_call_secrets(&self, profile_id: &str) -> Result<CallSecrets, WebError> {
        let name = vec![TAVILY_API_KEY.to_owned()];
        let (_, api_key) = self
            .inner
            .profiles
            .first_secret_snapshot(profile_id, &name, true)?
            .ok_or(WebError::MissingSecret)?;
        let mut redaction = self.inner.profiles.secret_redaction_snapshots(profile_id)?;
        if !redaction
            .iter()
            .any(|secret| secret.expose_secret() == api_key.expose_secret())
        {
            redaction.push(api_key.clone());
        }
        Ok(CallSecrets { api_key, redaction })
    }

    async fn acquire_permits(
        &self,
        profile_id: &str,
        control: &ToolExecutionControl,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), WebError> {
        let global =
            Arc::clone(GLOBAL_GATE.get_or_init(|| Arc::new(Semaphore::new(GLOBAL_CONCURRENCY))));
        let profile = {
            let mut gates = self
                .inner
                .profile_gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(
                gates
                    .entry(profile_id.to_owned())
                    .or_insert_with(|| Arc::new(Semaphore::new(PROFILE_CONCURRENCY))),
            )
        };

        loop {
            check_control(control)?;
            match global.clone().try_acquire_owned() {
                Ok(global_permit) => match profile.clone().try_acquire_owned() {
                    Ok(profile_permit) => return Ok((global_permit, profile_permit)),
                    Err(TryAcquireError::NoPermits) => drop(global_permit),
                    Err(TryAcquireError::Closed) => return Err(WebError::Busy),
                },
                Err(TryAcquireError::NoPermits) => {}
                Err(TryAcquireError::Closed) => return Err(WebError::Busy),
            }
            controlled_sleep(CONTROL_POLL_INTERVAL, control).await?;
        }
    }

    async fn post_json(
        &self,
        endpoint: &str,
        body: &JsonValue,
        api_key: &SecretString,
        control: &ToolExecutionControl,
    ) -> Result<Vec<u8>, WebError> {
        check_control(control)?;
        let client = self.provider_client(control).await?;
        let url = self
            .inner
            .base_url
            .join(endpoint)
            .map_err(|_| WebError::Unavailable)?;
        let mut payload = body.clone();
        payload
            .as_object_mut()
            .ok_or(WebError::InvalidArguments)?
            .insert(
                "api_key".to_owned(),
                JsonValue::String(api_key.expose_secret().to_owned()),
            );

        let response = await_controlled(
            client
                .post(url)
                .header(ACCEPT, "application/json")
                .json(&payload)
                .send(),
            control,
            FIRST_BYTE_TIMEOUT,
        )
        .await?
        .map_err(|_| WebError::Transport)?;
        read_bounded_json_response(response, control).await
    }

    async fn provider_client(&self, control: &ToolExecutionControl) -> Result<&Client, WebError> {
        match &self.inner.client {
            ProviderClient::Runtime(client) => {
                client
                    .get_or_try_init(|| build_pinned_provider_client(&self.inner.base_url, control))
                    .await
            }
            #[cfg(any(test, debug_assertions))]
            ProviderClient::Test(client) => Ok(client),
            ProviderClient::Unavailable => Err(WebError::Unavailable),
        }
    }

    async fn normalize_search_results(
        &self,
        results: impl Iterator<Item = TavilySearchResult>,
        secrets: &[SecretString],
        control: &ToolExecutionControl,
    ) -> Result<Vec<NormalizedSearchResult>, WebError> {
        let mut normalized = Vec::new();
        let mut budget = StringBudget::new(MAX_NORMALIZED_STRING_COST);
        for result in results {
            check_control(control)?;
            let url = match self
                .validate_public_url(&result.url, secrets, control)
                .await
            {
                Ok(url) => url,
                Err(WebError::UnsafeInput) => continue,
                Err(error) => return Err(error),
            };
            if !budget.take_whole(&url) {
                break;
            }
            let title = budget.take(&clean_external_collapsed(&result.title, secrets), 2_000);
            let description =
                budget.take(&clean_external_collapsed(&result.content, secrets), 8_000);
            normalized.push(NormalizedSearchResult {
                position: normalized.len() + 1,
                title,
                url,
                description,
            });
        }
        Ok(normalized)
    }

    async fn normalize_extract_results(
        &self,
        slots: &[ExtractInputSlot],
        response: TavilyExtractResponse,
        char_limit: usize,
        secrets: &[SecretString],
        control: &ToolExecutionControl,
    ) -> Result<Vec<NormalizedExtractResult>, WebError> {
        let requested: std::collections::HashSet<&str> = slots
            .iter()
            .filter_map(|slot| match slot {
                ExtractInputSlot::Safe(url) => Some(url.as_str()),
                ExtractInputSlot::Blocked(_) => None,
            })
            .collect();
        let mut prepared = HashMap::<String, PreparedExtractResult>::new();
        let mut failures = std::collections::HashSet::<String>::new();

        for result in response.results {
            check_control(control)?;
            let raw_url = result.url;
            let canonical_hint = Url::parse(&raw_url).ok().map(|url| url.to_string());
            let url = match self.validate_public_url(&raw_url, secrets, control).await {
                Ok(url) => url,
                Err(WebError::UnsafeInput) => {
                    if let Some(url) = canonical_hint.filter(|url| requested.contains(url.as_str()))
                    {
                        failures.insert(url);
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !requested.contains(url.as_str()) {
                continue;
            }
            let content = result.raw_content.or(result.content).unwrap_or_default();
            prepared
                .entry(url)
                .or_insert_with(|| PreparedExtractResult {
                    title: clean_external(&result.title, secrets),
                    content: truncate_chars(&clean_external(&content, secrets), char_limit),
                });
        }

        for failed in response.failed_results {
            if let Some(url) = failed.url.and_then(|url| Url::parse(&url).ok()) {
                let url = url.to_string();
                if requested.contains(url.as_str()) {
                    failures.insert(url);
                }
            }
        }
        for failed in response.failed_urls {
            if let Some(url) = failed.as_str().and_then(|url| Url::parse(url).ok()) {
                let url = url.to_string();
                if requested.contains(url.as_str()) {
                    failures.insert(url);
                }
            }
        }

        let mut normalized = Vec::with_capacity(slots.len());
        let mut budget = StringBudget::new(MAX_NORMALIZED_STRING_COST);
        for slot in slots {
            check_control(control)?;
            match slot {
                ExtractInputSlot::Blocked(url) => {
                    let url = budget.take(&clean_external_collapsed(url, secrets), 8_192);
                    normalized.push(NormalizedExtractResult::error(
                        url,
                        "Blocked by network safety policy",
                    ));
                }
                ExtractInputSlot::Safe(url) => {
                    if !budget.take_whole(url) {
                        normalized.push(NormalizedExtractResult::error(
                            String::new(),
                            "Output budget exceeded",
                        ));
                        continue;
                    }
                    if let Some(result) = prepared.get(url) {
                        let title = budget.take(&result.title, 2_000);
                        let content = budget.take(&result.content, budget.remaining());
                        normalized.push(NormalizedExtractResult {
                            url: url.clone(),
                            title,
                            content,
                            error: None,
                        });
                    } else {
                        let error = if failures.contains(url) {
                            "Extraction failed"
                        } else {
                            "Extract provider returned no result"
                        };
                        normalized.push(NormalizedExtractResult::error(url.clone(), error));
                    }
                }
            }
        }
        Ok(normalized)
    }

    async fn validate_public_url(
        &self,
        raw: &str,
        secrets: &[SecretString],
        control: &ToolExecutionControl,
    ) -> Result<String, WebError> {
        if raw.is_empty() || raw.len() > 8_192 || raw.chars().any(char::is_control) {
            return Err(WebError::UnsafeInput);
        }
        scan_sensitive(raw, secrets)?;
        let decoded = repeatedly_percent_decode(raw);
        if decoded.chars().any(char::is_control) {
            return Err(WebError::UnsafeInput);
        }
        let url = Url::parse(raw).map_err(|_| WebError::UnsafeInput)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host().is_none()
        {
            return Err(WebError::UnsafeInput);
        }
        reject_sensitive_query(&url)?;

        let host = url.host().ok_or(WebError::UnsafeInput)?;
        match host {
            Host::Ipv4(address) => ensure_public_ip(IpAddr::V4(address))?,
            Host::Ipv6(address) => ensure_public_ip(IpAddr::V6(address))?,
            Host::Domain(domain) => {
                reject_special_hostname(domain)?;
                let port = url.port_or_known_default().ok_or(WebError::UnsafeInput)?;
                let addresses = await_controlled(lookup_host((domain, port)), control, DNS_TIMEOUT)
                    .await?
                    .map_err(|_| WebError::UnsafeInput)?;
                let mut found = false;
                for address in addresses {
                    found = true;
                    ensure_public_ip(address.ip())?;
                }
                if !found {
                    return Err(WebError::UnsafeInput);
                }
            }
        }
        Ok(url.to_string())
    }
}

struct CallSecrets {
    api_key: SecretString,
    redaction: Vec<SecretString>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractInput {
    urls: Vec<String>,
    #[serde(default)]
    char_limit: Option<usize>,
}

#[derive(Default, Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    results: Vec<TavilySearchResult>,
}

#[derive(Deserialize)]
struct TavilySearchResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

#[derive(Default, Deserialize)]
struct TavilyExtractResponse {
    #[serde(default)]
    results: Vec<TavilyExtractResult>,
    #[serde(default)]
    failed_results: Vec<TavilyFailedResult>,
    #[serde(default)]
    failed_urls: Vec<JsonValue>,
}

#[derive(Deserialize)]
struct TavilyExtractResult {
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    raw_content: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct TavilyFailedResult {
    #[serde(default)]
    url: Option<String>,
}

enum ExtractInputSlot {
    Safe(String),
    Blocked(String),
}

struct PreparedExtractResult {
    title: String,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedSearchResult {
    position: usize,
    title: String,
    url: String,
    description: String,
}

#[derive(Serialize)]
struct NormalizedExtractResult {
    url: String,
    title: String,
    content: String,
    error: Option<String>,
}

impl NormalizedExtractResult {
    fn error(url: String, message: &str) -> Self {
        Self {
            url,
            title: String::new(),
            content: String::new(),
            error: Some(message.to_owned()),
        }
    }
}

fn strict_arguments<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, WebError> {
    if raw.is_empty() || raw.len() > MAX_ARGUMENT_BYTES {
        return Err(WebError::InvalidArguments);
    }
    serde_json::from_str(raw).map_err(|_| WebError::InvalidArguments)
}

fn validate_query(query: &str) -> Result<(), WebError> {
    let chars = query.chars().count();
    if chars == 0 || chars > 4_000 || query.trim().is_empty() || query.chars().any(char::is_control)
    {
        Err(WebError::InvalidArguments)
    } else {
        Ok(())
    }
}

fn effective_tavily_ready(provider: Option<&str>, status: WebProviderStatus) -> bool {
    provider == Some("tavily") && status == WebProviderStatus::Ready
}

fn require_effective_tavily(
    provider: Option<&str>,
    status: WebProviderStatus,
) -> Result<(), WebError> {
    if effective_tavily_ready(provider, status) {
        Ok(())
    } else if status == WebProviderStatus::MissingSecret {
        Err(WebError::MissingSecret)
    } else {
        Err(WebError::Unavailable)
    }
}

#[cfg(any(test, debug_assertions))]
fn normalized_test_provider_base_url(value: &str) -> Result<Url, WebError> {
    let mut url = Url::parse(value).map_err(|_| WebError::Unavailable)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host().is_none()
    {
        return Err(WebError::Unavailable);
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebError::Unavailable);
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

async fn build_pinned_provider_client(
    base_url: &Url,
    control: &ToolExecutionControl,
) -> Result<Client, WebError> {
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_HTTP_TIMEOUT)
        .https_only(true)
        .user_agent("SynthChat-Hermes-Rust/0.1 web-tavily");

    match base_url.host().ok_or(WebError::Unavailable)? {
        Host::Ipv4(address) => {
            ensure_public_ip(IpAddr::V4(address)).map_err(|_| WebError::Unavailable)?;
        }
        Host::Ipv6(address) => {
            ensure_public_ip(IpAddr::V6(address)).map_err(|_| WebError::Unavailable)?;
        }
        Host::Domain(domain) => {
            reject_special_hostname(domain).map_err(|_| WebError::Unavailable)?;
            let port = base_url
                .port_or_known_default()
                .ok_or(WebError::Unavailable)?;
            let resolved = await_controlled(lookup_host((domain, port)), control, DNS_TIMEOUT)
                .await?
                .map_err(|_| WebError::Unavailable)?;
            let addresses = validated_provider_addresses(resolved)?;
            builder = builder.resolve_to_addrs(domain, &addresses);
        }
    }

    builder.build().map_err(|_| WebError::Unavailable)
}

fn validated_provider_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, WebError> {
    let mut validated = Vec::new();
    for address in addresses {
        ensure_public_ip(address.ip()).map_err(|_| WebError::Unavailable)?;
        if !validated.contains(&address) {
            validated.push(address);
        }
        if validated.len() > 64 {
            return Err(WebError::Unavailable);
        }
    }
    if validated.is_empty() {
        return Err(WebError::Unavailable);
    }
    Ok(validated)
}

async fn read_bounded_json_response(
    response: Response,
    control: &ToolExecutionControl,
) -> Result<Vec<u8>, WebError> {
    if !response.status().is_success() {
        return Err(map_provider_status(response.status()));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(WebError::InvalidResponse)?;
    if !is_json_content_type(content_type) {
        return Err(WebError::InvalidResponse);
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(WebError::InvalidResponse);
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let chunk = await_controlled(stream.next(), control, IDLE_TIMEOUT).await?;
        match chunk {
            Some(Ok(chunk)) => {
                let next_len = bytes
                    .len()
                    .checked_add(chunk.len())
                    .ok_or(WebError::InvalidResponse)?;
                if next_len > MAX_RESPONSE_BYTES {
                    return Err(WebError::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Err(_)) => return Err(WebError::Transport),
            None => break,
        }
    }
    if bytes.is_empty() {
        return Err(WebError::InvalidResponse);
    }
    Ok(bytes)
}

fn map_provider_status(status: StatusCode) -> WebError {
    if matches!(status.as_u16(), 401 | 403) {
        WebError::MissingSecret
    } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        WebError::Transport
    } else {
        WebError::InvalidResponse
    }
}

fn is_json_content_type(value: &str) -> bool {
    let media_type = value.split(';').next().unwrap_or_default().trim();
    media_type.eq_ignore_ascii_case("application/json")
        || (media_type.len() > "application/+json".len()
            && media_type.to_ascii_lowercase().starts_with("application/")
            && media_type.to_ascii_lowercase().ends_with("+json"))
}

async fn await_controlled<F, T>(
    future: F,
    control: &ToolExecutionControl,
    timeout: Duration,
) -> Result<T, WebError>
where
    F: Future<Output = T>,
{
    check_control(control)?;
    tokio::pin!(future);
    let timeout = tokio::time::sleep(timeout);
    tokio::pin!(timeout);
    let mut interval = tokio::time::interval(CONTROL_POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            output = &mut future => return Ok(output),
            () = &mut timeout => return Err(WebError::Transport),
            _ = interval.tick() => check_control(control)?,
        }
    }
}

async fn controlled_sleep(
    duration: Duration,
    control: &ToolExecutionControl,
) -> Result<(), WebError> {
    await_controlled(
        tokio::time::sleep(duration),
        control,
        duration + CONTROL_POLL_INTERVAL,
    )
    .await
}

fn check_control(control: &ToolExecutionControl) -> Result<(), WebError> {
    control.check().map_err(|error| match error {
        ToolExecutionControlError::Cancelled => WebError::Cancelled,
        ToolExecutionControlError::DeadlineExceeded => WebError::DeadlineExceeded,
    })
}

fn scan_sensitive(value: &str, secrets: &[SecretString]) -> Result<(), WebError> {
    let mut candidate = value.to_owned();
    for _ in 0..3 {
        if contains_secret_or_credential(&candidate, secrets) {
            return Err(WebError::UnsafeInput);
        }
        let decoded = percent_decode(&candidate);
        if decoded == candidate {
            break;
        }
        candidate = decoded;
    }
    Ok(())
}

fn contains_secret_or_credential(value: &str, secrets: &[SecretString]) -> bool {
    secrets.iter().any(|secret| {
        let secret = secret.expose_secret();
        !secret.is_empty() && value.contains(secret)
    }) || KNOWN_CREDENTIAL.is_match(value)
}

fn repeatedly_percent_decode(value: &str) -> String {
    let mut decoded = value.to_owned();
    for _ in 0..3 {
        let next = percent_decode(&decoded);
        if next == decoded {
            break;
        }
        decoded = next;
    }
    decoded
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn reject_sensitive_query(url: &Url) -> Result<(), WebError> {
    for (name, _) in url.query_pairs() {
        let normalized: String = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if matches!(
            normalized.as_str(),
            "key"
                | "apikey"
                | "xapikey"
                | "token"
                | "accesstoken"
                | "refreshtoken"
                | "auth"
                | "authorization"
                | "password"
                | "passwd"
                | "secret"
                | "signature"
                | "sig"
                | "credential"
                | "credentials"
                | "cookie"
                | "session"
        ) {
            return Err(WebError::UnsafeInput);
        }
    }
    Ok(())
}

fn reject_special_hostname(host: &str) -> Result<(), WebError> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host == "metadata.google.internal"
        || host == "metadata.aws.internal"
        || host == "instance-data"
        || host.ends_with(".home.arpa")
    {
        Err(WebError::UnsafeInput)
    } else {
        Ok(())
    }
}

fn ensure_public_ip(address: IpAddr) -> Result<(), WebError> {
    let public = match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    };
    if public {
        Ok(())
    } else {
        Err(WebError::UnsafeInput)
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224
        || (a == 255 && b == 255 && c == 255 && d == 255))
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if address.to_ipv4_mapped().is_some() {
        return false;
    }
    let segments = address.segments();
    if !(0x2000..=0x3fff).contains(&segments[0]) {
        return false;
    }
    // Teredo, benchmarking, ORCHID, documentation, and 3fff::/20 documentation.
    let reserved_2001 = segments[0] == 0x2001
        && (matches!(segments[1], 0 | 2 | 0x0db8) || (0x0010..=0x002f).contains(&segments[1]));
    if reserved_2001 || (segments[0] == 0x3fff && segments[1] <= 0x0fff) || segments[0] == 0x2002 {
        return false;
    }
    true
}

fn clean_external(value: &str, secrets: &[SecretString]) -> String {
    let without_ansi = ANSI_ESCAPE.replace_all(value, " ");
    let without_data = INLINE_DATA.replace_all(&without_ansi, " [removed encoded data] ");
    let mut cleaned = String::with_capacity(without_data.len().min(MAX_PROVIDER_CONTENT_BYTES));
    let mut previous_was_cr = false;
    for character in without_data.chars() {
        match character {
            '\r' => {
                cleaned.push('\n');
                previous_was_cr = true;
            }
            '\n' => {
                if !previous_was_cr {
                    cleaned.push('\n');
                }
                previous_was_cr = false;
            }
            '\t' => {
                cleaned.push_str("    ");
                previous_was_cr = false;
            }
            character if character.is_control() => {
                previous_was_cr = false;
            }
            character if character.is_whitespace() && character != ' ' => {
                cleaned.push(' ');
                previous_was_cr = false;
            }
            character => {
                cleaned.push(character);
                previous_was_cr = false;
            }
        }
    }
    let mut cleaned = cleaned.trim().to_owned();
    for secret in secrets {
        let secret = secret.expose_secret();
        if !secret.is_empty() && cleaned.contains(secret) {
            cleaned = cleaned.replace(secret, "[REDACTED]");
        }
    }
    KNOWN_CREDENTIAL
        .replace_all(&cleaned, "[REDACTED]")
        .into_owned()
}

fn clean_external_collapsed(value: &str, secrets: &[SecretString]) -> String {
    clean_external(value, secrets)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        value.chars().take(limit).collect()
    }
}

struct StringBudget {
    remaining: usize,
}

impl StringBudget {
    fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    fn remaining(&self) -> usize {
        self.remaining
    }

    fn take(&mut self, value: &str, maximum: usize) -> String {
        let mut output = String::new();
        let mut bytes = 0;
        let mut cost = 0;
        for character in value.chars() {
            let character_bytes = character.len_utf8();
            let character_cost = json_escaped_character_cost(character);
            if bytes + character_bytes > maximum || cost + character_cost > self.remaining {
                break;
            }
            output.push(character);
            bytes += character_bytes;
            cost += character_cost;
        }
        self.remaining -= cost;
        output
    }

    fn take_whole(&mut self, value: &str) -> bool {
        let cost = json_escaped_string_cost(value);
        if cost > self.remaining {
            false
        } else {
            self.remaining -= cost;
            true
        }
    }
}

fn json_escaped_string_cost(value: &str) -> usize {
    value.chars().map(json_escaped_character_cost).sum()
}

fn json_escaped_character_cost(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
        character if character.is_control() => 6,
        character => character.len_utf8(),
    }
}

fn build_output(
    normalized: JsonValue,
    input_summary: String,
    result_summary: String,
) -> Result<WebExecutionOutput, WebError> {
    let raw_result_json =
        serde_json::to_string(&normalized).map_err(|_| WebError::InvalidResponse)?;
    if raw_result_json.len() > MAX_PROVIDER_CONTENT_BYTES {
        return Err(WebError::InvalidResponse);
    }
    Ok(WebExecutionOutput {
        provider_content: raw_result_json.clone(),
        raw_result_json,
        input_summary,
        result_summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Vec<SecretString> {
        vec![SecretString::from("tvly-unit-test-secret".to_owned())]
    }

    #[test]
    fn strict_search_schema_rejects_unknown_and_wrong_scalar_types() {
        assert!(strict_arguments::<SearchInput>(r#"{"query":"rust"}"#).is_ok());
        assert!(strict_arguments::<SearchInput>(r#"{"query":["rust"]}"#).is_err());
        assert!(strict_arguments::<SearchInput>(r#"{"query":"rust","extra":1}"#).is_err());
        assert!(strict_arguments::<SearchInput>(r#"{"query":"rust","limit":1.5}"#).is_err());
    }

    #[test]
    fn strict_extract_schema_rejects_unknown_and_non_array_urls() {
        assert!(strict_arguments::<ExtractInput>(r#"{"urls":["https://example.com"]}"#).is_ok());
        assert!(strict_arguments::<ExtractInput>(r#"{"urls":"https://example.com"}"#).is_err());
        assert!(strict_arguments::<ExtractInput>(r#"{"urls":[],"depth":"advanced"}"#).is_err());
    }

    #[test]
    fn query_validation_uses_unicode_length_and_rejects_controls() {
        assert!(validate_query("rust").is_ok());
        assert!(validate_query("   ").is_err());
        assert!(validate_query("a\nsecret").is_err());
        assert!(validate_query(&"界".repeat(4_000)).is_ok());
        assert!(validate_query(&"界".repeat(4_001)).is_err());
    }

    #[test]
    fn sensitive_scan_checks_exact_nested_encoding_and_known_tokens() {
        let secrets = secrets();
        assert!(scan_sensitive("ordinary search", &secrets).is_ok());
        assert!(scan_sensitive("tvly-unit-test-secret", &secrets).is_err());
        assert!(scan_sensitive("tvly%252Dunit%252Dtest%252Dsecret", &secrets).is_err());
        assert!(scan_sensitive("Authorization: Bearer abcdefghijklmnop", &[]).is_err());
        assert!(scan_sensitive("AKIAABCDEFGHIJKLMNOP", &[]).is_err());
    }

    #[test]
    fn sensitive_query_names_are_rejected_after_url_decoding() {
        assert!(
            reject_sensitive_query(&Url::parse("https://example.com/?page=1").unwrap()).is_ok()
        );
        assert!(
            reject_sensitive_query(&Url::parse("https://example.com/?api%5Fkey=x").unwrap())
                .is_err()
        );
        assert!(
            reject_sensitive_query(&Url::parse("https://example.com/?access-token=x").unwrap())
                .is_err()
        );
    }

    #[test]
    fn ipv4_policy_rejects_every_special_family() {
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.0.9",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!is_public_ipv4(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ipv4("8.8.8.8".parse().unwrap()));
        assert!(is_public_ipv4("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn ipv6_policy_rejects_local_mapped_transition_and_documentation() {
        for address in [
            "::",
            "::1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "2002:0808:0808::1",
            "3fff::1",
        ] {
            assert!(!is_public_ipv6(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ipv6("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn hostname_policy_rejects_local_and_metadata_names() {
        for hostname in [
            "localhost",
            "service.localhost",
            "printer.local",
            "metadata.google.internal",
            "instance-data",
            "router.home.arpa",
        ] {
            assert!(reject_special_hostname(hostname).is_err(), "{hostname}");
        }
        assert!(reject_special_hostname("example.com").is_ok());
    }

    #[test]
    fn sanitizer_removes_controls_ansi_inline_data_and_secrets() {
        let input = "\u{1b}[31mred\u{1b}[0m\0\n tvly-unit-test-secret data:text/plain;base64,QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=";
        let cleaned = clean_external(input, &secrets());
        assert!(!cleaned.contains('\u{1b}'));
        assert!(!cleaned.contains('\0'));
        assert!(!cleaned.contains("tvly-unit-test-secret"));
        assert!(!cleaned.contains("data:text"));
        assert!(cleaned.contains("[REDACTED]"));
        assert!(cleaned.contains("[removed encoded data]"));
    }

    #[test]
    fn sanitizer_preserves_markdown_and_code_line_structure() {
        let input = "# Heading\r\n\r\n```rust\nfn main() {\n    println!(\"ok\");\n}\n```";
        let cleaned = clean_external(input, &[]);
        assert!(cleaned.contains("# Heading\n\n```rust\n"));
        assert!(cleaned.contains("    println!"));
        assert!(cleaned.ends_with("\n```"));
        assert!(!clean_external_collapsed(input, &[]).contains('\n'));
    }

    #[test]
    fn escaped_cost_budget_preserves_utf8_boundaries_and_charges_json_escapes() {
        let mut budget = StringBudget::new(5);
        assert_eq!(budget.take("a界b", usize::MAX), "a界b");
        assert_eq!(budget.remaining(), 0);

        let mut escaped = StringBudget::new(5);
        assert_eq!(escaped.take("a\n\"b", usize::MAX), "a\n\"");
        assert_eq!(escaped.remaining(), 0);
        assert_eq!(truncate_chars("a界b", 2), "a界");
    }

    #[test]
    fn normalized_output_is_bounded_and_marked_untrusted() {
        let output = build_output(
            json!({"externalUntrusted": true, "results": []}),
            "Web search requested".to_owned(),
            "Found 0 web result(s)".to_owned(),
        )
        .unwrap();
        assert!(output.raw_result_json.len() <= MAX_PROVIDER_CONTENT_BYTES);
        assert_eq!(output.raw_result_json, output.provider_content);
        assert!(output.raw_result_json.contains("externalUntrusted"));
        assert!(!output.input_summary.contains("query"));
    }

    #[test]
    fn content_type_accepts_json_and_structured_json_only() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type(
            "Application/Problem+Json; charset=utf-8"
        ));
        assert!(!is_json_content_type("text/json"));
        assert!(!is_json_content_type("text/html"));
    }

    #[test]
    fn test_provider_endpoint_accepts_only_http_family_urls_without_embedded_state() {
        assert!(normalized_test_provider_base_url("http://127.0.0.1:3000/mock").is_ok());
        assert!(normalized_test_provider_base_url("ftp://127.0.0.1/mock").is_err());
        assert!(normalized_test_provider_base_url("http://user@127.0.0.1/mock").is_err());
    }

    #[test]
    fn provider_dns_requires_every_address_to_be_public_before_pinning() {
        let public = "93.184.216.34:443".parse().unwrap();
        let second_public = "1.1.1.1:443".parse().unwrap();
        assert_eq!(
            validated_provider_addresses([public, public, second_public]).unwrap(),
            vec![public, second_public]
        );
        assert!(
            validated_provider_addresses([
                "93.184.216.34:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ])
            .is_err()
        );
        assert!(validated_provider_addresses([]).is_err());
    }

    #[tokio::test]
    async fn url_policy_accepts_public_literals_and_rejects_private_literals() {
        let root = tempfile::tempdir().unwrap();
        let service = WebService::unavailable(Arc::new(ProfileService::without_credential_store(
            root.path().to_owned(),
        )));
        let control = ToolExecutionControl::new(std::time::Instant::now() + Duration::from_secs(5));
        assert!(
            service
                .validate_public_url("https://93.184.216.34/path", &[], &control)
                .await
                .is_ok()
        );
        assert!(
            service
                .validate_public_url("http://127.0.0.1/", &[], &control)
                .await
                .is_err()
        );
        assert!(
            service
                .validate_public_url("http://[::ffff:8.8.8.8]/", &[], &control)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancelled_control_stops_before_network_work() {
        let control = ToolExecutionControl::new(std::time::Instant::now() + Duration::from_secs(5));
        control.cancel();
        assert!(matches!(check_control(&control), Err(WebError::Cancelled)));
        assert!(matches!(
            controlled_sleep(Duration::from_secs(1), &control).await,
            Err(WebError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn deadline_and_operation_timeout_are_distinct_static_errors() {
        let expired = ToolExecutionControl::new(std::time::Instant::now());
        assert!(matches!(
            await_controlled(
                std::future::pending::<()>(),
                &expired,
                Duration::from_secs(1)
            )
            .await,
            Err(WebError::DeadlineExceeded)
        ));

        let active = ToolExecutionControl::new(std::time::Instant::now() + Duration::from_secs(5));
        assert!(matches!(
            await_controlled(
                std::future::pending::<()>(),
                &active,
                Duration::from_millis(25)
            )
            .await,
            Err(WebError::Transport)
        ));
    }

    #[test]
    fn summaries_never_need_raw_inputs() {
        let search = format!("Found {} web result(s)", 3);
        let extract = format!("Extracted {} web page(s)", 2);
        assert_eq!(search, "Found 3 web result(s)");
        assert_eq!(extract, "Extracted 2 web page(s)");
    }

    #[test]
    fn profile_errors_are_collapsed_at_the_web_boundary() {
        let error = WebError::from(ProfileError::InvalidProfileId);
        assert!(matches!(error, WebError::Profile));
        assert_eq!(error.to_string(), "profile operation failed");
        assert_eq!(format!("{error:?}"), "profile operation failed");
    }
}
