use std::{
    collections::{BTreeSet, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use reqwest::{
    Client, Method, Response, StatusCode,
    header::{
        ACCEPT, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap,
        HeaderName, HeaderValue, LOCATION,
    },
    redirect::Policy,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value as JsonValue, json};
use tokio::{net::lookup_host, time::Instant};
use url::{Host, Url};

use super::{
    DiscoveredTool, MAX_MCP_LIST_PAGES, MAX_MCP_RESULT_BYTES, MAX_MCP_TOOLS, MAX_RPC_MESSAGE_BYTES,
    MCP_PROTOCOL_VERSION, McpRuntimeError, McpTransport, StoredServer, await_runtime,
    check_control, parse_discovered_tool,
};
use crate::tools::ToolExecutionControl;

const STREAMABLE_PROTOCOL_VERSION: &str = "2025-06-18";
const STREAMABLE_PROTOCOL_VERSIONS: [&str; 2] = ["2025-03-26", "2025-06-18"];
const MAX_REDIRECTS: usize = 5;
const MAX_RUNTIME_URL_BYTES: usize = 4_096;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_SSE_EVENT_NAME_BYTES: usize = 64;
const MAX_SSE_SESSION_BYTES: usize = 8 * 1024 * 1024;
const MAX_DNS_ADDRESSES: usize = 32;
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

static MCP_SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");
static MCP_PROTOCOL_VERSION_HEADER: HeaderName = HeaderName::from_static("mcp-protocol-version");

pub(super) async fn discover(
    server: &StoredServer,
    bearer: Option<SecretString>,
) -> Result<Vec<DiscoveredTool>, McpRuntimeError> {
    let timeout = Duration::from_secs(server.timeout_seconds);
    let mut client = RemoteClient::connect(server, bearer, timeout, None).await?;
    let result = async {
        client.initialize(timeout, None).await?;
        client.list_tools(timeout, None).await
    }
    .await;
    client.close().await;
    result
}

pub(super) async fn call(
    server: &StoredServer,
    bearer: Option<SecretString>,
    tool_name: &str,
    arguments: JsonValue,
    control: &ToolExecutionControl,
) -> Result<JsonValue, McpRuntimeError> {
    let timeout = Duration::from_secs(server.timeout_seconds);
    let mut client = RemoteClient::connect(server, bearer, timeout, Some(control)).await?;
    let result = async {
        client.initialize(timeout, Some(control)).await?;
        client
            .call_tool(tool_name, arguments, timeout, control)
            .await
    }
    .await;
    client.close().await;
    result
}

enum RemoteClient {
    Streamable(Box<StreamableClient>),
    LegacySse(Box<LegacySseClient>),
}

impl RemoteClient {
    async fn connect(
        server: &StoredServer,
        bearer: Option<SecretString>,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<Self, McpRuntimeError> {
        let endpoint = server
            .url
            .as_deref()
            .ok_or(McpRuntimeError::Configuration)
            .and_then(parse_runtime_url)?;
        match server.transport {
            McpTransport::StreamableHttp => Ok(Self::Streamable(Box::new(StreamableClient::new(
                endpoint, bearer,
            )))),
            McpTransport::Sse => LegacySseClient::connect(endpoint, bearer, timeout, control)
                .await
                .map(|client| Self::LegacySse(Box::new(client))),
            McpTransport::Stdio => Err(McpRuntimeError::Configuration),
        }
    }

    async fn initialize(
        &mut self,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        let requested_version = match self {
            Self::Streamable(_) => STREAMABLE_PROTOCOL_VERSION,
            Self::LegacySse(_) => MCP_PROTOCOL_VERSION,
        };
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": requested_version,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "synthchat",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
                timeout,
                control,
            )
            .await?;
        let object = result.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
        let negotiated = object
            .get("protocolVersion")
            .and_then(JsonValue::as_str)
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        let supported = match self {
            Self::Streamable(_) => STREAMABLE_PROTOCOL_VERSIONS.contains(&negotiated),
            Self::LegacySse(_) => negotiated == MCP_PROTOCOL_VERSION,
        };
        if !supported || !object.get("capabilities").is_some_and(JsonValue::is_object) {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        if let Self::Streamable(client) = self {
            client.protocol_version = Some(negotiated.to_owned());
        }
        self.notify("notifications/initialized", json!({}), timeout, control)
            .await
    }

    async fn list_tools(
        &mut self,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<Vec<DiscoveredTool>, McpRuntimeError> {
        let mut cursor: Option<String> = None;
        let mut tools = Vec::new();
        let mut names = BTreeSet::new();
        for _ in 0..MAX_MCP_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let result = self.request("tools/list", params, timeout, control).await?;
            let object = result.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
            let page = object
                .get("tools")
                .and_then(JsonValue::as_array)
                .ok_or(McpRuntimeError::InvalidProtocol)?;
            for value in page {
                let tool = parse_discovered_tool(value)?;
                if !names.insert(tool.name.clone()) || tools.len() >= MAX_MCP_TOOLS {
                    return Err(McpRuntimeError::InvalidProtocol);
                }
                tools.push(tool);
            }
            cursor = match object.get("nextCursor") {
                None | Some(JsonValue::Null) => None,
                Some(JsonValue::String(value))
                    if !value.is_empty()
                        && value.len() <= 1024
                        && !value.chars().any(char::is_control) =>
                {
                    Some(value.clone())
                }
                _ => return Err(McpRuntimeError::InvalidProtocol),
            };
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(McpRuntimeError::InvalidProtocol)
    }

    async fn call_tool(
        &mut self,
        name: &str,
        arguments: JsonValue,
        timeout: Duration,
        control: &ToolExecutionControl,
    ) -> Result<JsonValue, McpRuntimeError> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                timeout,
                Some(control),
            )
            .await?;
        let object = result.as_object().ok_or(McpRuntimeError::InvalidResult)?;
        if !object.get("content").is_some_and(JsonValue::is_array)
            || object
                .get("isError")
                .is_some_and(|value| !value.is_boolean())
            || serde_json::to_vec(&result)
                .map_err(|_| McpRuntimeError::InvalidResult)?
                .len()
                > MAX_MCP_RESULT_BYTES
        {
            return Err(McpRuntimeError::InvalidResult);
        }
        Ok(result)
    }

    async fn request(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<JsonValue, McpRuntimeError> {
        match self {
            Self::Streamable(client) => client.request(method, params, timeout, control).await,
            Self::LegacySse(client) => client.request(method, params, timeout, control).await,
        }
    }

    async fn notify(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        match self {
            Self::Streamable(client) => client.notify(method, params, timeout, control).await,
            Self::LegacySse(client) => client.notify(method, params, timeout, control).await,
        }
    }

    async fn close(&mut self) {
        if let Self::Streamable(client) = self {
            client.close().await;
        }
    }
}

struct StreamableClient {
    configured_endpoint: Url,
    bearer: Option<SecretString>,
    session: Option<SessionBinding>,
    protocol_version: Option<String>,
    next_id: u64,
}

struct SessionBinding {
    origin: Url,
    value: String,
}

impl StreamableClient {
    fn new(configured_endpoint: Url, bearer: Option<SecretString>) -> Self {
        Self {
            configured_endpoint,
            bearer,
            session: None,
            protocol_version: None,
            next_id: 1,
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<JsonValue, McpRuntimeError> {
        check_control(control)?;
        let id = self.allocate_id()?;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let deadline = Instant::now() + timeout;
        let (mut response, final_url) = self
            .send(
                Method::POST,
                Some(encode_rpc(&payload)?),
                "application/json, text/event-stream",
                deadline,
                control,
            )
            .await?;
        self.observe_session(&response, &final_url)?;
        if response.status() != StatusCode::OK {
            return Err(McpRuntimeError::Transport);
        }
        match response_media_type(response.headers())? {
            ResponseMedia::Json => {
                let bytes = read_bounded_body(&mut response, deadline, control).await?;
                let message = parse_rpc_bytes(&bytes)?;
                match classify_rpc(message, id)? {
                    RpcInbound::Result(result) => Ok(result),
                    RpcInbound::Error => Err(McpRuntimeError::Transport),
                    RpcInbound::ServerRequest(request_id) => {
                        self.reject_server_request(request_id, deadline, control)
                            .await?;
                        Err(McpRuntimeError::InvalidProtocol)
                    }
                    RpcInbound::Notification => Err(McpRuntimeError::InvalidProtocol),
                }
            }
            ResponseMedia::EventStream => {
                let mut decoder = SseDecoder::default();
                loop {
                    let event = next_sse_event(&mut response, &mut decoder, deadline, control)
                        .await?
                        .ok_or(McpRuntimeError::InvalidProtocol)?;
                    if event.event != "message" {
                        continue;
                    }
                    let message = parse_rpc_bytes(event.data.as_bytes())?;
                    match classify_rpc(message, id)? {
                        RpcInbound::Result(result) => return Ok(result),
                        RpcInbound::Error => return Err(McpRuntimeError::Transport),
                        RpcInbound::Notification => {}
                        RpcInbound::ServerRequest(request_id) => {
                            self.reject_server_request(request_id, deadline, control)
                                .await?;
                        }
                    }
                }
            }
        }
    }

    async fn notify(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        let payload = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send_one_way(payload, Instant::now() + timeout, control)
            .await
    }

    fn allocate_id(&mut self) -> Result<u64, McpRuntimeError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        Ok(id)
    }

    fn observe_session(
        &mut self,
        response: &Response,
        final_url: &Url,
    ) -> Result<(), McpRuntimeError> {
        let candidate = optional_single_header(response.headers(), &MCP_SESSION_ID)?
            .map(valid_session_id)
            .transpose()?;
        match (&self.session, candidate) {
            (None, Some(value)) => {
                self.session = Some(SessionBinding {
                    origin: final_url.clone(),
                    value,
                });
            }
            (Some(existing), Some(value)) if existing.value != value => {
                return Err(McpRuntimeError::InvalidProtocol);
            }
            _ => {}
        }
        if let Some(session) = self.session.as_ref()
            && !same_origin(&session.origin, final_url)
        {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        Ok(())
    }

    async fn reject_server_request(
        &mut self,
        id: JsonValue,
        deadline: Instant,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        self.send_one_way(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            }),
            deadline,
            control,
        )
        .await
    }

    async fn send_one_way(
        &mut self,
        payload: JsonValue,
        deadline: Instant,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        let (response, final_url) = self
            .send(
                Method::POST,
                Some(encode_rpc(&payload)?),
                "application/json, text/event-stream",
                deadline,
                control,
            )
            .await?;
        self.observe_session(&response, &final_url)?;
        if matches!(
            response.status(),
            StatusCode::ACCEPTED | StatusCode::NO_CONTENT
        ) {
            Ok(())
        } else {
            Err(McpRuntimeError::Transport)
        }
    }

    async fn send(
        &self,
        method: Method,
        body: Option<Vec<u8>>,
        accept: &'static str,
        deadline: Instant,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(Response, Url), McpRuntimeError> {
        send_http_request(
            self.configured_endpoint.clone(),
            method,
            body,
            accept,
            RequestCredentials {
                bearer_origin: &self.configured_endpoint,
                bearer: self.bearer.as_ref(),
                session: self
                    .session
                    .as_ref()
                    .map(|session| (&session.origin, session.value.as_str())),
                protocol_version: self.protocol_version.as_deref(),
            },
            deadline,
            control,
        )
        .await
    }

    async fn close(&mut self) {
        if self.session.is_none() {
            return;
        }
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        let _ = self
            .send(Method::DELETE, None, "application/json", deadline, None)
            .await;
        self.session = None;
    }
}

struct LegacySseClient {
    configured_endpoint: Url,
    bearer: Option<SecretString>,
    response: Response,
    decoder: SseDecoder,
    post_endpoint: Url,
    next_id: u64,
}

impl LegacySseClient {
    async fn connect(
        configured_endpoint: Url,
        bearer: Option<SecretString>,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<Self, McpRuntimeError> {
        let deadline = Instant::now() + timeout;
        let (mut response, final_url) = send_http_request(
            configured_endpoint.clone(),
            Method::GET,
            None,
            "text/event-stream",
            RequestCredentials {
                bearer_origin: &configured_endpoint,
                bearer: bearer.as_ref(),
                session: None,
                protocol_version: None,
            },
            deadline,
            control,
        )
        .await?;
        if response.status() != StatusCode::OK
            || response_media_type(response.headers())? != ResponseMedia::EventStream
        {
            return Err(McpRuntimeError::Transport);
        }
        let mut decoder = SseDecoder::default();
        let event = next_sse_event(&mut response, &mut decoder, deadline, control)
            .await?
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        if event.event != "endpoint" || event.data.is_empty() {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        let post_endpoint = final_url
            .join(&event.data)
            .map_err(|_| McpRuntimeError::InvalidProtocol)?;
        validate_endpoint_url(&post_endpoint)?;
        if !same_origin(&post_endpoint, &final_url) {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        // Validate and pin the negotiated endpoint independently from the GET
        // connection before any credential or JSON-RPC payload is sent to it.
        let _ = resolve_target(&post_endpoint, deadline, control).await?;
        Ok(Self {
            configured_endpoint,
            bearer,
            response,
            decoder,
            post_endpoint,
            next_id: 1,
        })
    }

    async fn request(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<JsonValue, McpRuntimeError> {
        check_control(control)?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        let deadline = Instant::now() + timeout;
        self.post(
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            deadline,
            control,
        )
        .await?;
        loop {
            let event = next_sse_event(&mut self.response, &mut self.decoder, deadline, control)
                .await?
                .ok_or(McpRuntimeError::InvalidProtocol)?;
            if event.event == "endpoint" {
                return Err(McpRuntimeError::InvalidProtocol);
            }
            if event.event != "message" {
                continue;
            }
            let message = parse_rpc_bytes(event.data.as_bytes())?;
            match classify_rpc(message, id)? {
                RpcInbound::Result(result) => return Ok(result),
                RpcInbound::Error => return Err(McpRuntimeError::Transport),
                RpcInbound::Notification => {}
                RpcInbound::ServerRequest(request_id) => {
                    self.post(
                        json!({
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "error": { "code": -32601, "message": "Method not found" }
                        }),
                        deadline,
                        control,
                    )
                    .await?;
                }
            }
        }
    }

    async fn notify(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        self.post(
            json!({ "jsonrpc": "2.0", "method": method, "params": params }),
            Instant::now() + timeout,
            control,
        )
        .await
    }

    async fn post(
        &self,
        payload: JsonValue,
        deadline: Instant,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        let (response, final_url) = send_http_request(
            self.post_endpoint.clone(),
            Method::POST,
            Some(encode_rpc(&payload)?),
            "application/json",
            RequestCredentials {
                bearer_origin: &self.configured_endpoint,
                bearer: self.bearer.as_ref(),
                session: None,
                protocol_version: None,
            },
            deadline,
            control,
        )
        .await?;
        if !same_origin(&final_url, &self.post_endpoint)
            || !matches!(
                response.status(),
                StatusCode::ACCEPTED | StatusCode::NO_CONTENT
            )
        {
            return Err(McpRuntimeError::Transport);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct RequestCredentials<'a> {
    bearer_origin: &'a Url,
    bearer: Option<&'a SecretString>,
    session: Option<(&'a Url, &'a str)>,
    protocol_version: Option<&'a str>,
}

async fn send_http_request(
    mut url: Url,
    method: Method,
    body: Option<Vec<u8>>,
    accept: &'static str,
    credentials: RequestCredentials<'_>,
    deadline: Instant,
    control: Option<&ToolExecutionControl>,
) -> Result<(Response, Url), McpRuntimeError> {
    if body
        .as_ref()
        .is_some_and(|body| body.len() > MAX_RPC_MESSAGE_BYTES)
    {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    for redirects in 0..=MAX_REDIRECTS {
        check_control(control)?;
        let target = resolve_target(&url, deadline, control).await?;
        let client = target.client()?;
        let mut request = client
            .request(method.clone(), target.url.clone())
            .header(ACCEPT, accept);
        if let Some(body) = body.as_ref() {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone());
        }
        if same_origin(&target.url, credentials.bearer_origin)
            && let Some(bearer) = credentials.bearer
        {
            request = request.header(AUTHORIZATION, format!("Bearer {}", bearer.expose_secret()));
        }
        if let Some((origin, value)) = credentials.session
            && same_origin(&target.url, origin)
        {
            request = request.header(&MCP_SESSION_ID, value);
        }
        if let Some(version) = credentials.protocol_version {
            request = request.header(&MCP_PROTOCOL_VERSION_HEADER, version);
        }
        let response = await_runtime(
            async { request.send().await.map_err(|_| McpRuntimeError::Transport) },
            deadline,
            control,
        )
        .await?;
        if !response.status().is_redirection() {
            validate_response_framing(response.headers())?;
            return Ok((response, target.url));
        }
        if redirects == MAX_REDIRECTS || !redirect_preserves_method(response.status(), &method) {
            return Err(McpRuntimeError::Transport);
        }
        let location = required_single_header(response.headers(), &LOCATION)?
            .to_str()
            .map_err(|_| McpRuntimeError::Transport)?;
        url = target
            .url
            .join(location)
            .map_err(|_| McpRuntimeError::Transport)?;
        validate_endpoint_url(&url)?;
    }
    Err(McpRuntimeError::Transport)
}

fn redirect_preserves_method(status: StatusCode, method: &Method) -> bool {
    matches!(
        status,
        StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
    ) || (*method == Method::GET
        && matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND))
}

struct ResolvedTarget {
    url: Url,
    domain: Option<String>,
    addresses: Vec<SocketAddr>,
}

impl ResolvedTarget {
    fn client(&self) -> Result<Client, McpRuntimeError> {
        let mut builder = Client::builder()
            .redirect(Policy::none())
            // Ambient proxy settings would bypass the pinned DNS destination
            // and could receive a Profile-scoped bearer token.
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent("SynthChat-Hermes-Rust/0.1 mcp");
        if let Some(domain) = self.domain.as_deref() {
            builder = builder.resolve_to_addrs(domain, &self.addresses);
        }
        builder.build().map_err(|_| McpRuntimeError::Transport)
    }
}

async fn resolve_target(
    url: &Url,
    deadline: Instant,
    control: Option<&ToolExecutionControl>,
) -> Result<ResolvedTarget, McpRuntimeError> {
    validate_endpoint_url(url)?;
    let host = url.host().ok_or(McpRuntimeError::Transport)?;
    let port = url
        .port_or_known_default()
        .ok_or(McpRuntimeError::Transport)?;
    match host {
        Host::Ipv4(address) => {
            validate_address(IpAddr::V4(address), url.scheme(), address.is_loopback())?;
            Ok(ResolvedTarget {
                url: url.clone(),
                domain: None,
                addresses: Vec::new(),
            })
        }
        Host::Ipv6(address) => {
            validate_address(IpAddr::V6(address), url.scheme(), address.is_loopback())?;
            Ok(ResolvedTarget {
                url: url.clone(),
                domain: None,
                addresses: Vec::new(),
            })
        }
        Host::Domain(domain) => {
            let loopback_name = is_loopback_name(domain);
            reject_special_hostname(domain, loopback_name)?;
            let dns_deadline = deadline.min(Instant::now() + DNS_TIMEOUT);
            let resolved = await_runtime(
                async {
                    lookup_host((domain, port))
                        .await
                        .map_err(|_| McpRuntimeError::Transport)
                },
                dns_deadline,
                control,
            )
            .await?;
            let mut addresses = resolved
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if addresses.is_empty() || addresses.len() > MAX_DNS_ADDRESSES {
                return Err(McpRuntimeError::Transport);
            }
            for address in &addresses {
                validate_address(address.ip(), url.scheme(), loopback_name)?;
            }
            addresses.sort();
            Ok(ResolvedTarget {
                url: url.clone(),
                domain: Some(domain.to_owned()),
                addresses,
            })
        }
    }
}

fn parse_runtime_url(raw: &str) -> Result<Url, McpRuntimeError> {
    let url = Url::parse(raw).map_err(|_| McpRuntimeError::Configuration)?;
    validate_endpoint_url(&url).map_err(|_| McpRuntimeError::Configuration)?;
    if url.query().is_some() {
        return Err(McpRuntimeError::Configuration);
    }
    Ok(url)
}

fn validate_endpoint_url(url: &Url) -> Result<(), McpRuntimeError> {
    if url.as_str().len() > MAX_RUNTIME_URL_BYTES
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
    {
        return Err(McpRuntimeError::Transport);
    }
    let loopback = match url.host().ok_or(McpRuntimeError::Transport)? {
        Host::Domain(domain) => is_loopback_name(domain),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    };
    if url.scheme() == "http" && !loopback {
        return Err(McpRuntimeError::Transport);
    }
    Ok(())
}

fn validate_address(
    address: IpAddr,
    scheme: &str,
    allow_loopback: bool,
) -> Result<(), McpRuntimeError> {
    if address.is_loopback() {
        return if allow_loopback {
            Ok(())
        } else {
            Err(McpRuntimeError::Transport)
        };
    }
    let public = match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    };
    if scheme == "https" && public {
        Ok(())
    } else {
        Err(McpRuntimeError::Transport)
    }
}

fn is_loopback_name(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.');
    domain.eq_ignore_ascii_case("localhost") || domain.to_ascii_lowercase().ends_with(".localhost")
}

fn reject_special_hostname(domain: &str, loopback_name: bool) -> Result<(), McpRuntimeError> {
    if loopback_name {
        return Ok(());
    }
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.ends_with(".local")
        || domain.ends_with(".internal")
        || domain.ends_with(".home.arpa")
        || domain == "metadata.google.internal"
        || domain == "metadata.aws.internal"
        || domain == "instance-data"
    {
        Err(McpRuntimeError::Transport)
    } else {
        Ok(())
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
    let reserved_2001 = segments[0] == 0x2001
        && (matches!(segments[1], 0 | 2 | 0x0db8) || (0x0010..=0x002f).contains(&segments[1]));
    !(reserved_2001 || (segments[0] == 0x3fff && segments[1] <= 0x0fff) || segments[0] == 0x2002)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn validate_response_framing(headers: &HeaderMap) -> Result<(), McpRuntimeError> {
    if let Some(value) = optional_single_header(headers, &CONTENT_ENCODING)?
        && !value
            .to_str()
            .is_ok_and(|value| value.eq_ignore_ascii_case("identity"))
    {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    if let Some(value) = optional_single_header(headers, &CONTENT_LENGTH)? {
        let length = value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        if length > MAX_RPC_MESSAGE_BYTES as u64 {
            return Err(McpRuntimeError::InvalidProtocol);
        }
    }
    Ok(())
}

fn required_single_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> Result<&'a HeaderValue, McpRuntimeError> {
    optional_single_header(headers, name)?.ok_or(McpRuntimeError::Transport)
}

fn optional_single_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> Result<Option<&'a HeaderValue>, McpRuntimeError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    Ok(value)
}

fn valid_session_id(value: &HeaderValue) -> Result<String, McpRuntimeError> {
    let value = value
        .to_str()
        .map_err(|_| McpRuntimeError::InvalidProtocol)?;
    if value.is_empty()
        || value.len() > MAX_SESSION_ID_BYTES
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    Ok(value.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseMedia {
    Json,
    EventStream,
}

fn response_media_type(headers: &HeaderMap) -> Result<ResponseMedia, McpRuntimeError> {
    let value = required_single_header(headers, &CONTENT_TYPE)?
        .to_str()
        .map_err(|_| McpRuntimeError::InvalidProtocol)?;
    match value.split(';').next().map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("application/json") => Ok(ResponseMedia::Json),
        Some(value) if value.eq_ignore_ascii_case("text/event-stream") => {
            Ok(ResponseMedia::EventStream)
        }
        _ => Err(McpRuntimeError::InvalidProtocol),
    }
}

async fn read_bounded_body(
    response: &mut Response,
    deadline: Instant,
    control: Option<&ToolExecutionControl>,
) -> Result<Vec<u8>, McpRuntimeError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = await_runtime(
        async {
            response
                .chunk()
                .await
                .map_err(|_| McpRuntimeError::Transport)
        },
        deadline,
        control,
    )
    .await?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RPC_MESSAGE_BYTES {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    Ok(bytes)
}

fn encode_rpc(value: &JsonValue) -> Result<Vec<u8>, McpRuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(|_| McpRuntimeError::InvalidProtocol)?;
    if bytes.len() > MAX_RPC_MESSAGE_BYTES {
        Err(McpRuntimeError::InvalidProtocol)
    } else {
        Ok(bytes)
    }
}

fn parse_rpc_bytes(bytes: &[u8]) -> Result<JsonValue, McpRuntimeError> {
    if bytes.is_empty() || bytes.len() > MAX_RPC_MESSAGE_BYTES {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    serde_json::from_slice(bytes).map_err(|_| McpRuntimeError::InvalidProtocol)
}

enum RpcInbound {
    Result(JsonValue),
    Error,
    Notification,
    ServerRequest(JsonValue),
}

fn classify_rpc(message: JsonValue, expected_id: u64) -> Result<RpcInbound, McpRuntimeError> {
    let object = message
        .as_object()
        .ok_or(McpRuntimeError::InvalidProtocol)?;
    if object.get("jsonrpc").and_then(JsonValue::as_str) != Some("2.0") {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    if object.contains_key("method") {
        if !object.get("method").is_some_and(JsonValue::is_string) {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        return match object.get("id") {
            None => Ok(RpcInbound::Notification),
            Some(id) if valid_rpc_id(id) => Ok(RpcInbound::ServerRequest(id.clone())),
            _ => Err(McpRuntimeError::InvalidProtocol),
        };
    }
    if object.get("id") != Some(&json!(expected_id)) {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(RpcInbound::Result(result.clone())),
        (None, Some(error)) if valid_rpc_error(error) => Ok(RpcInbound::Error),
        _ => Err(McpRuntimeError::InvalidProtocol),
    }
}

fn valid_rpc_id(value: &JsonValue) -> bool {
    match value {
        JsonValue::String(value) => {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        }
        JsonValue::Number(value) => value.as_i64().is_some() || value.as_u64().is_some(),
        _ => false,
    }
}

fn valid_rpc_error(value: &JsonValue) -> bool {
    value.as_object().is_some_and(|error| {
        error.get("code").and_then(JsonValue::as_i64).is_some()
            && error.get("message").is_some_and(JsonValue::is_string)
    })
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    event_name: Option<String>,
    data: String,
    events: VecDeque<SseEvent>,
    total_bytes: usize,
    finished: bool,
}

struct SseEvent {
    event: String,
    data: String,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<(), McpRuntimeError> {
        self.total_bytes = self
            .total_bytes
            .checked_add(chunk.len())
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        if self.total_bytes > MAX_SSE_SESSION_BYTES {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        self.buffer.extend_from_slice(chunk);
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=position).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line)?;
        }
        if self.buffer.len() > MAX_RPC_MESSAGE_BYTES {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), McpRuntimeError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.process_line(&line)?;
        }
        self.dispatch();
        Ok(())
    }

    fn process_line(&mut self, bytes: &[u8]) -> Result<(), McpRuntimeError> {
        let line = std::str::from_utf8(bytes).map_err(|_| McpRuntimeError::InvalidProtocol)?;
        if line.is_empty() {
            self.dispatch();
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "event" => {
                if value.len() > MAX_SSE_EVENT_NAME_BYTES
                    || value.is_empty()
                    || value.chars().any(char::is_control)
                {
                    return Err(McpRuntimeError::InvalidProtocol);
                }
                self.event_name = Some(value.to_owned());
            }
            "data" => {
                let additional = value.len() + usize::from(!self.data.is_empty());
                if self.data.len().saturating_add(additional) > MAX_RPC_MESSAGE_BYTES {
                    return Err(McpRuntimeError::InvalidProtocol);
                }
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self) {
        if self.event_name.is_none() && self.data.is_empty() {
            return;
        }
        self.events.push_back(SseEvent {
            event: self
                .event_name
                .take()
                .unwrap_or_else(|| "message".to_owned()),
            data: std::mem::take(&mut self.data),
        });
    }
}

async fn next_sse_event(
    response: &mut Response,
    decoder: &mut SseDecoder,
    deadline: Instant,
    control: Option<&ToolExecutionControl>,
) -> Result<Option<SseEvent>, McpRuntimeError> {
    loop {
        if let Some(event) = decoder.events.pop_front() {
            return Ok(Some(event));
        }
        let chunk = await_runtime(
            async {
                response
                    .chunk()
                    .await
                    .map_err(|_| McpRuntimeError::Transport)
            },
            deadline,
            control,
        )
        .await?;
        match chunk {
            Some(chunk) => decoder.push(&chunk)?,
            None => {
                decoder.finish()?;
                return Ok(decoder.events.pop_front());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{Response, StatusCode, header},
        routing::post,
    };

    use super::*;

    #[test]
    fn sse_decoder_handles_chunking_multiline_and_bounds() {
        let mut decoder = SseDecoder::default();
        decoder
            .push(b": heartbeat\r\nevent: message\r\nda")
            .unwrap();
        decoder.push(b"ta: {\"one\":\r\ndata: 1}\r\n\r\n").unwrap();
        let event = decoder.events.pop_front().unwrap();
        assert_eq!(event.event, "message");
        assert_eq!(event.data, "{\"one\":\n1}");

        let mut oversized = SseDecoder::default();
        assert!(
            oversized
                .push(&vec![b'x'; MAX_RPC_MESSAGE_BYTES + 1])
                .is_err()
        );
    }

    #[test]
    fn public_address_policy_rejects_reserved_ranges() {
        for address in [
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.168.1.1",
            "198.51.100.2",
            "203.0.113.4",
        ] {
            assert!(!is_public_ipv4(address.parse().unwrap()));
        }
        assert!(is_public_ipv4("8.8.8.8".parse().unwrap()));
        assert!(!is_public_ipv6("2001:db8::1".parse().unwrap()));
        assert!(is_public_ipv6("2606:4700:4700::1111".parse().unwrap()));
    }

    #[derive(Clone, Default)]
    struct CancellationFixture;

    async fn cancellation_post(
        State(_fixture): State<CancellationFixture>,
        Json(request): Json<JsonValue>,
    ) -> Response<Body> {
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let id = request.get("id").cloned();
        match method {
            "initialize" => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"protocolVersion": "2025-06-18", "capabilities": {}}
                    })
                    .to_string(),
                ))
                .unwrap(),
            "notifications/initialized" => Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Body::empty())
                .unwrap(),
            "tools/call" => {
                pending::<()>().await;
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty())
                    .unwrap()
            }
            _ => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::empty())
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn streamable_request_observes_in_flight_cancellation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/mcp", post(cancellation_post))
                    .with_state(CancellationFixture),
            )
            .await
            .unwrap();
        });
        let mut client = RemoteClient::Streamable(Box::new(StreamableClient::new(
            Url::parse(&format!("http://{address}/mcp")).unwrap(),
            None,
        )));
        client
            .initialize(Duration::from_secs(5), None)
            .await
            .unwrap();
        let control = ToolExecutionControl::new(std::time::Instant::now() + Duration::from_secs(5));
        let request = client.request(
            "tools/call",
            json!({"name": "echo", "arguments": {}}),
            Duration::from_secs(5),
            Some(&control),
        );
        tokio::pin!(request);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(75)) => control.cancel(),
            result = &mut request => panic!("request completed before cancellation: {result:?}"),
        }
        assert_eq!(request.await, Err(McpRuntimeError::Cancelled));
        server.abort();
    }
}
