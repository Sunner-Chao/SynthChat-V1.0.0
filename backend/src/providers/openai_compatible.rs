use std::{
    collections::{BTreeMap, HashSet},
    future::pending,
    mem, str,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, CONTENT_TYPE},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};

use super::{
    ProviderError, ProviderEvent, ProviderFinish, ProviderMessage, ProviderRequest,
    ProviderToolCall, ProviderToolDefinition, ProviderTransport, ProviderTurn, ProviderUsage,
};

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PENDING_EVENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_DEFINITIONS: usize = 128;
const MAX_TOOL_CALLS: usize = 64;
const MAX_TOOL_CALL_ID_BYTES: usize = 512;
const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 64 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    client: Client,
    idle_timeout: Duration,
}

impl OpenAiCompatibleProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }

    pub fn with_client_and_timeout(client: Client, idle_timeout: Duration) -> Self {
        Self {
            client,
            idle_timeout,
        }
    }

    pub async fn stream_chat(
        &self,
        request: ProviderRequest,
        events: mpsc::Sender<ProviderEvent>,
        cancelled: watch::Receiver<bool>,
    ) -> Result<ProviderTurn, ProviderError> {
        self.stream_chat_inner(request, events, cancelled).await
    }

    async fn stream_chat_inner(
        &self,
        request: ProviderRequest,
        events: mpsc::Sender<ProviderEvent>,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<ProviderTurn, ProviderError> {
        validate_request(&request)?;
        if *cancelled.borrow_and_update() {
            return Err(ProviderError::Cancelled);
        }

        let payload = ChatCompletionRequest {
            model: &request.model,
            messages: request.messages.iter().map(WireMessage::from).collect(),
            tools: (!request.tools.is_empty())
                .then(|| request.tools.iter().map(WireToolDefinition::from).collect()),
            tool_choice: (!request.tools.is_empty()).then_some("auto"),
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            reasoning_effort: request.reasoning_effort.as_deref(),
        };
        let mut builder = self
            .client
            .post(request.url.clone())
            .header(ACCEPT, "text/event-stream")
            .json(&payload);
        if let Some(secret) = request.secret.as_ref() {
            builder = builder.bearer_auth(secret.expose_secret());
        }

        let response = tokio::select! {
            _ = cancellation_requested(&mut cancelled) => {
                return Err(ProviderError::Cancelled);
            }
            result = timeout(self.idle_timeout, builder.send()) => {
                match result {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => return Err(map_transport_error(&error)),
                    Err(_) => return Err(ProviderError::Timeout),
                }
            }
        };
        validate_response(&response)?;

        consume_stream(
            response.bytes_stream().boxed(),
            &events,
            &mut cancelled,
            self.idle_timeout,
            request.tools.iter().map(|tool| tool.name.clone()).collect(),
        )
        .await
    }
}

impl Default for OpenAiCompatibleProvider {
    fn default() -> Self {
        Self::with_client(Client::new())
    }
}

#[async_trait]
impl ProviderTransport for OpenAiCompatibleProvider {
    async fn stream_chat(
        &self,
        request: ProviderRequest,
        events: mpsc::Sender<ProviderEvent>,
        cancelled: watch::Receiver<bool>,
    ) -> Result<ProviderTurn, ProviderError> {
        self.stream_chat_inner(request, events, cancelled).await
    }
}

fn validate_request(request: &ProviderRequest) -> Result<(), ProviderError> {
    if request.provider_id.trim().is_empty()
        || request.model.trim().is_empty()
        || request.messages.is_empty()
        || !is_http_url(&request.base_url)
        || !is_http_url(&request.url)
        || request.messages.iter().any(invalid_message)
        || request.tools.len() > MAX_TOOL_DEFINITIONS
        || invalid_tool_definitions(&request.tools)
        || request.reasoning_effort.as_deref().is_some_and(|effort| {
            !matches!(effort, "minimal" | "low" | "medium" | "high" | "xhigh")
        })
    {
        return Err(ProviderError::InvalidConfiguration);
    }
    Ok(())
}

fn invalid_message(message: &ProviderMessage) -> bool {
    match message {
        ProviderMessage::System { .. } | ProviderMessage::User { .. } => false,
        ProviderMessage::Assistant {
            content,
            tool_calls,
        } => {
            content.is_none() && tool_calls.is_empty()
                || tool_calls.len() > MAX_TOOL_CALLS
                || tool_calls.iter().any(invalid_complete_tool_call)
                || has_duplicate_call_ids(tool_calls)
        }
        ProviderMessage::Tool {
            tool_call_id,
            content: _,
        } => invalid_tool_call_id(tool_call_id),
        ProviderMessage::Unsupported { .. } => true,
    }
}

fn invalid_complete_tool_call(call: &ProviderToolCall) -> bool {
    invalid_tool_call_id(&call.id)
        || invalid_tool_name(&call.name)
        || call.arguments_json.len() > MAX_TOOL_ARGUMENT_BYTES
        || match serde_json::from_str::<serde_json::Value>(&call.arguments_json) {
            Ok(value) => !value.is_object(),
            Err(_) => true,
        }
}

fn has_duplicate_call_ids(calls: &[ProviderToolCall]) -> bool {
    let mut ids = HashSet::with_capacity(calls.len());
    calls.iter().any(|call| !ids.insert(call.id.as_str()))
}

fn invalid_tool_definitions(tools: &[ProviderToolDefinition]) -> bool {
    let mut names = HashSet::with_capacity(tools.len());
    tools.iter().any(|tool| {
        invalid_tool_name(&tool.name)
            || !names.insert(tool.name.as_str())
            || tool.description.len() > MAX_TOOL_DESCRIPTION_BYTES
            || !tool.parameters.is_object()
    })
}

fn invalid_tool_call_id(id: &str) -> bool {
    id.is_empty() || id.len() > MAX_TOOL_CALL_ID_BYTES || id.chars().any(char::is_control)
}

fn invalid_tool_name(name: &str) -> bool {
    name.is_empty()
        || name.len() > MAX_TOOL_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_http_url(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn validate_response(response: &Response) -> Result<(), ProviderError> {
    if !response.status().is_success() {
        return Err(match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Authentication,
            StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited,
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => ProviderError::Timeout,
            status if status.is_client_error() => ProviderError::RequestRejected,
            _ => ProviderError::Unavailable,
        });
    }

    let is_event_stream = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
    if !is_event_stream {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(())
}

fn map_transport_error(error: &reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Unavailable
    }
}

async fn consume_stream<S, B>(
    mut stream: S,
    events: &mpsc::Sender<ProviderEvent>,
    cancelled: &mut watch::Receiver<bool>,
    idle_timeout: Duration,
    allowed_tools: HashSet<String>,
) -> Result<ProviderTurn, ProviderError>
where
    S: Stream<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut decoder = SseDecoder::default();
    let mut state = StreamState::new(allowed_tools);

    loop {
        let next = tokio::select! {
            _ = cancellation_requested(cancelled) => {
                return Err(ProviderError::Cancelled);
            }
            result = timeout(idle_timeout, stream.next()) => {
                match result {
                    Ok(value) => value,
                    Err(_) => return Err(ProviderError::Timeout),
                }
            }
        };

        match next {
            Some(Ok(chunk)) => {
                for payload in decoder.push(chunk.as_ref())? {
                    if process_payload(&payload, events, cancelled, &mut state).await? {
                        return state.finish();
                    }
                }
            }
            Some(Err(error)) => return Err(map_transport_error(&error)),
            None => {
                for payload in decoder.finish()? {
                    if process_payload(&payload, events, cancelled, &mut state).await? {
                        return state.finish();
                    }
                }
                return state.finish();
            }
        }
    }
}

async fn process_payload(
    payload: &[u8],
    events: &mpsc::Sender<ProviderEvent>,
    cancelled: &mut watch::Receiver<bool>,
    state: &mut StreamState,
) -> Result<bool, ProviderError> {
    let payload = str::from_utf8(payload)
        .map_err(|_| ProviderError::InvalidResponse)?
        .trim();
    if payload == "[DONE]" {
        return Ok(true);
    }
    if payload.is_empty() {
        return Ok(false);
    }

    let chunk: ChatCompletionChunk =
        serde_json::from_str(payload).map_err(|_| ProviderError::InvalidResponse)?;
    if chunk.error.is_some() {
        return Err(ProviderError::StreamFailed);
    }

    for choice in chunk.choices {
        if choice.index.unwrap_or(0) != 0 || state.finish_reason.is_some() {
            return Err(ProviderError::InvalidResponse);
        }
        if let Some(reasoning) = choice
            .delta
            .reasoning_content
            .or(choice.delta.reasoning)
            .or(choice.delta.reasoning_text)
            .filter(|value| !value.is_empty())
        {
            send_event(events, cancelled, ProviderEvent::ReasoningDelta(reasoning)).await?;
        }
        if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
            send_event(events, cancelled, ProviderEvent::TextDelta(content)).await?;
        }
        for tool_call in choice.delta.tool_calls {
            state.push_tool_call(tool_call)?;
        }
        if let Some(reason) = choice.finish_reason {
            state.finish_reason = Some(FinishReason::try_from(reason.as_str())?);
        }
    }

    if let Some(usage) = chunk.usage {
        let usage = usage.merge_with(&state.usage)?;
        if !state.saw_usage || usage != state.usage {
            send_event(events, cancelled, ProviderEvent::Usage(usage.clone())).await?;
            state.usage = usage;
            state.saw_usage = true;
        }
    }
    Ok(false)
}

async fn send_event(
    events: &mpsc::Sender<ProviderEvent>,
    cancelled: &mut watch::Receiver<bool>,
    event: ProviderEvent,
) -> Result<(), ProviderError> {
    tokio::select! {
        _ = cancellation_requested(cancelled) => Err(ProviderError::Cancelled),
        result = events.send(event) => result.map_err(|_| ProviderError::Cancelled),
    }
}

async fn cancellation_requested(cancelled: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow_and_update() {
            return;
        }
        if cancelled.changed().await.is_err() {
            pending::<()>().await;
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireToolDefinition<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    stream: bool,
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireMessage<'a> {
    Text(WireTextMessage<'a>),
    Assistant(WireAssistantMessage<'a>),
    Tool(WireToolMessage<'a>),
}

#[derive(Serialize)]
struct WireTextMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct WireAssistantMessage<'a> {
    role: &'static str,
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Serialize)]
struct WireToolMessage<'a> {
    role: &'static str,
    tool_call_id: &'a str,
    content: &'a str,
}

impl<'a> From<&'a ProviderMessage> for WireMessage<'a> {
    fn from(message: &'a ProviderMessage) -> Self {
        match message {
            ProviderMessage::System { content } => Self::Text(WireTextMessage {
                role: "system",
                content,
            }),
            ProviderMessage::User { content } => Self::Text(WireTextMessage {
                role: "user",
                content,
            }),
            ProviderMessage::Assistant {
                content,
                tool_calls,
            } => Self::Assistant(WireAssistantMessage {
                role: "assistant",
                content: content.as_deref(),
                tool_calls: tool_calls.iter().map(WireToolCall::from).collect(),
            }),
            ProviderMessage::Tool {
                tool_call_id,
                content,
            } => Self::Tool(WireToolMessage {
                role: "tool",
                tool_call_id,
                content,
            }),
            ProviderMessage::Unsupported { role, content } => {
                Self::Text(WireTextMessage { role, content })
            }
        }
    }
}

#[derive(Serialize)]
struct WireToolDefinition<'a> {
    r#type: &'static str,
    function: WireFunctionDefinition<'a>,
}

#[derive(Serialize)]
struct WireFunctionDefinition<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

impl<'a> From<&'a ProviderToolDefinition> for WireToolDefinition<'a> {
    fn from(tool: &'a ProviderToolDefinition) -> Self {
        Self {
            r#type: "function",
            function: WireFunctionDefinition {
                name: &tool.name,
                description: &tool.description,
                parameters: &tool.parameters,
                strict: tool.strict,
            },
        }
    }
}

#[derive(Serialize)]
struct WireToolCall {
    id: String,
    r#type: &'static str,
    function: WireFunctionCall,
}

#[derive(Serialize)]
struct WireFunctionCall {
    name: String,
    arguments: String,
}

impl From<&ProviderToolCall> for WireToolCall {
    fn from(call: &ProviderToolCall) -> Self {
        Self {
            id: call.id.clone(),
            r#type: "function",
            function: WireFunctionCall {
                name: call.name.clone(),
                arguments: call.arguments_json.clone(),
            },
        }
    }
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct WireChoice {
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_text: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCallDelta>,
}

#[derive(Deserialize)]
struct WireToolCallDelta {
    index: Option<u64>,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<WireFunctionCallDelta>,
}

#[derive(Deserialize)]
struct WireFunctionCallDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default, alias = "input_tokens")]
    prompt_tokens: Option<u64>,
    #[serde(default, alias = "output_tokens")]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    cost: Option<f64>,
}

impl WireUsage {
    fn merge_with(self, previous: &ProviderUsage) -> Result<ProviderUsage, ProviderError> {
        let prompt_tokens = self.prompt_tokens.unwrap_or(previous.prompt_tokens);
        let completion_tokens = self.completion_tokens.unwrap_or(previous.completion_tokens);
        let minimum_total = prompt_tokens
            .checked_add(completion_tokens)
            .ok_or(ProviderError::InvalidResponse)?;
        let total_tokens = self.total_tokens.unwrap_or(minimum_total);
        let cost = self.cost.or(previous.cost);

        if prompt_tokens < previous.prompt_tokens
            || completion_tokens < previous.completion_tokens
            || total_tokens < previous.total_tokens
            || total_tokens < minimum_total
            || cost.is_some_and(|value| !value.is_finite() || value < 0.0)
            || matches!((previous.cost, cost), (Some(old), Some(new)) if new < old)
        {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(ProviderUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost,
        })
    }
}

struct StreamState {
    usage: ProviderUsage,
    saw_usage: bool,
    finish_reason: Option<FinishReason>,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    allowed_tools: HashSet<String>,
}

impl StreamState {
    fn new(allowed_tools: HashSet<String>) -> Self {
        Self {
            usage: ProviderUsage::default(),
            saw_usage: false,
            finish_reason: None,
            tool_calls: BTreeMap::new(),
            allowed_tools,
        }
    }

    fn push_tool_call(&mut self, delta: WireToolCallDelta) -> Result<(), ProviderError> {
        let index = usize::try_from(delta.index.ok_or(ProviderError::InvalidResponse)?)
            .map_err(|_| ProviderError::InvalidResponse)?;
        if index >= MAX_TOOL_CALLS {
            return Err(ProviderError::InvalidResponse);
        }
        if self.tool_calls.len() >= MAX_TOOL_CALLS && !self.tool_calls.contains_key(&index) {
            return Err(ProviderError::InvalidResponse);
        }
        if delta.kind.as_deref().is_some_and(|kind| kind != "function") {
            return Err(ProviderError::InvalidResponse);
        }

        let call = self.tool_calls.entry(index).or_default();
        if delta.kind.as_deref() == Some("function") {
            call.saw_function_type = true;
        }
        if let Some(id) = delta.id {
            if invalid_tool_call_id(&id) || call.id.as_ref().is_some_and(|existing| existing != &id)
            {
                return Err(ProviderError::InvalidResponse);
            }
            call.id = Some(id);
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                if invalid_tool_name(&name)
                    || call.name.as_ref().is_some_and(|existing| existing != &name)
                {
                    return Err(ProviderError::InvalidResponse);
                }
                call.name = Some(name);
            }
            if let Some(arguments) = function.arguments {
                let next_length = call
                    .arguments
                    .len()
                    .checked_add(arguments.len())
                    .ok_or(ProviderError::InvalidResponse)?;
                if next_length > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(ProviderError::InvalidResponse);
                }
                call.arguments.push_str(&arguments);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<ProviderTurn, ProviderError> {
        if !self.saw_usage {
            return Err(ProviderError::InvalidResponse);
        }
        let reason = self.finish_reason.ok_or(ProviderError::InvalidResponse)?;
        let finish = match reason {
            FinishReason::ToolCalls => {
                if self.tool_calls.is_empty()
                    || self.tool_calls.keys().copied().ne(0..self.tool_calls.len())
                {
                    return Err(ProviderError::InvalidResponse);
                }
                let mut ids = HashSet::with_capacity(self.tool_calls.len());
                let mut completed = Vec::with_capacity(self.tool_calls.len());
                for partial in self.tool_calls.into_values() {
                    if !partial.saw_function_type {
                        return Err(ProviderError::InvalidResponse);
                    }
                    let id = partial.id.ok_or(ProviderError::InvalidResponse)?;
                    let name = partial.name.ok_or(ProviderError::InvalidResponse)?;
                    if !ids.insert(id.clone()) || !self.allowed_tools.contains(&name) {
                        return Err(ProviderError::InvalidResponse);
                    }
                    let arguments: serde_json::Value = serde_json::from_str(&partial.arguments)
                        .map_err(|_| ProviderError::InvalidResponse)?;
                    if !arguments.is_object() {
                        return Err(ProviderError::InvalidResponse);
                    }
                    completed.push(ProviderToolCall {
                        id,
                        name,
                        arguments_json: partial.arguments,
                    });
                }
                ProviderFinish::ToolCalls(completed)
            }
            FinishReason::Stop => {
                if !self.tool_calls.is_empty() {
                    return Err(ProviderError::InvalidResponse);
                }
                ProviderFinish::Stop
            }
            FinishReason::Length => {
                if !self.tool_calls.is_empty() {
                    return Err(ProviderError::InvalidResponse);
                }
                ProviderFinish::Length
            }
            FinishReason::ContentFilter => {
                if !self.tool_calls.is_empty() {
                    return Err(ProviderError::InvalidResponse);
                }
                ProviderFinish::ContentFilter
            }
        };
        Ok(ProviderTurn {
            usage: self.usage,
            finish,
        })
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    saw_function_type: bool,
}

#[derive(Clone, Copy)]
enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

impl TryFrom<&str> for FinishReason {
    type Error = ProviderError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "stop" => Ok(Self::Stop),
            "tool_calls" => Ok(Self::ToolCalls),
            "length" => Ok(Self::Length),
            "content_filter" => Ok(Self::ContentFilter),
            _ => Err(ProviderError::InvalidResponse),
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
    data_bytes: usize,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, ProviderError> {
        self.buffer.extend_from_slice(bytes);
        let mut completed = Vec::new();
        let mut line_start = 0;
        let mut index = 0;

        while index < self.buffer.len() {
            let separator_width = match self.buffer[index] {
                b'\n' => 1,
                b'\r' if index + 1 < self.buffer.len() && self.buffer[index + 1] == b'\n' => 2,
                b'\r' if index + 1 < self.buffer.len() => 1,
                b'\r' => break,
                _ => {
                    index += 1;
                    continue;
                }
            };
            let line = self.buffer[line_start..index].to_vec();
            self.process_line(&line, &mut completed)?;
            index += separator_width;
            line_start = index;
        }

        if line_start > 0 {
            self.buffer.drain(..line_start);
        }
        if self
            .buffer
            .len()
            .checked_add(self.data_bytes)
            .is_none_or(|size| size > MAX_PENDING_EVENT_BYTES)
        {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(completed)
    }

    fn finish(&mut self) -> Result<Vec<Vec<u8>>, ProviderError> {
        let mut completed = Vec::new();
        if self.buffer.last() == Some(&b'\r') {
            self.buffer.pop();
        }
        if !self.buffer.is_empty() {
            let line = mem::take(&mut self.buffer);
            self.process_line(&line, &mut completed)?;
        }
        self.dispatch(&mut completed)?;
        Ok(completed)
    }

    fn process_line(
        &mut self,
        line: &[u8],
        completed: &mut Vec<Vec<u8>>,
    ) -> Result<(), ProviderError> {
        if line.is_empty() {
            return self.dispatch(completed);
        }
        if line[0] == b':' {
            return Ok(());
        }

        let colon = line.iter().position(|byte| *byte == b':');
        let (field, mut value) = match colon {
            Some(index) => (&line[..index], &line[index + 1..]),
            None => (line, &[][..]),
        };
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        if field == b"data" {
            let separator = usize::from(!self.data_lines.is_empty());
            self.data_bytes = self
                .data_bytes
                .checked_add(value.len() + separator)
                .ok_or(ProviderError::InvalidResponse)?;
            if self.data_bytes > MAX_PENDING_EVENT_BYTES {
                return Err(ProviderError::InvalidResponse);
            }
            self.data_lines.push(value.to_vec());
        }
        Ok(())
    }

    fn dispatch(&mut self, completed: &mut Vec<Vec<u8>>) -> Result<(), ProviderError> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let mut payload = Vec::with_capacity(self.data_bytes);
        for (index, line) in self.data_lines.drain(..).enumerate() {
            if index > 0 {
                payload.push(b'\n');
            }
            payload.extend_from_slice(&line);
        }
        self.data_bytes = 0;
        completed.push(payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, sync::Arc};

    use axum::{
        Json, Router,
        body::{Body, Bytes},
        http::{HeaderMap, Response},
        routing::{get, post},
    };
    use futures_util::stream;
    use secrecy::SecretString;
    use serde_json::{Value, json};
    use tokio::{
        net::TcpListener,
        sync::{mpsc, oneshot, watch},
        task::JoinHandle,
        time::sleep,
    };
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn streams_split_utf8_crlf_reasoning_text_and_usage() {
        let (seen_tx, seen_rx) = oneshot::channel();
        let seen_tx = Arc::new(std::sync::Mutex::new(Some(seen_tx)));
        let app = Router::new().route(
            "/v1/chat/completions",
            post({
                let seen_tx = seen_tx.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| {
                    let seen_tx = seen_tx.clone();
                    async move {
                        if let Some(sender) = seen_tx.lock().unwrap().take() {
                            let authorization = headers
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or_default()
                                .to_owned();
                            let _ = sender.send((authorization, body));
                        }
                        one_byte_sse_response(concat!(
                            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"思考\"}}]}\r\n\r\n",
                            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5,\"cost\":0.1}}\r\n\r\n",
                            "data: [DONE]\r\n\r\n"
                        ))
                    }
                }
            }),
        );
        let (endpoint, server) = spawn_server(app).await;
        let provider = OpenAiCompatibleProvider::new();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let turn = provider
            .stream_chat(
                request(endpoint, Some("never-log-this-secret")),
                event_tx,
                cancel_rx,
            )
            .await
            .unwrap();

        assert_eq!(
            turn,
            ProviderTurn {
                usage: ProviderUsage {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    total_tokens: 5,
                    cost: Some(0.1),
                },
                finish: ProviderFinish::Stop,
            }
        );
        assert_eq!(
            event_rx.recv().await,
            Some(ProviderEvent::ReasoningDelta("思考".to_owned()))
        );
        assert_eq!(
            event_rx.recv().await,
            Some(ProviderEvent::TextDelta("你好".to_owned()))
        );
        assert_eq!(
            event_rx.recv().await,
            Some(ProviderEvent::Usage(turn.usage.clone()))
        );
        assert!(event_rx.recv().await.is_none());

        let (authorization, body) = seen_rx.await.unwrap();
        assert_eq!(authorization, "Bearer never-log-this-secret");
        assert_eq!(body["model"], "test-model");
        assert_eq!(
            body["messages"][0],
            json!({"role":"user","content":"hello"})
        );
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        abort_server(server).await;
    }

    #[tokio::test]
    async fn serializes_assistant_tool_calls_tool_results_and_definitions_exactly() {
        let (seen_tx, seen_rx) = oneshot::channel();
        let seen_tx = Arc::new(std::sync::Mutex::new(Some(seen_tx)));
        let app = Router::new().route(
            "/v1/chat/completions",
            post({
                let seen_tx = seen_tx.clone();
                move |Json(body): Json<Value>| {
                    let seen_tx = seen_tx.clone();
                    async move {
                        if let Some(sender) = seen_tx.lock().unwrap().take() {
                            let _ = sender.send(body);
                        }
                        one_byte_sse_response(concat!(
                            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":1,\"total_tokens\":9}}\n\n",
                            "data: [DONE]\n\n"
                        ))
                    }
                }
            }),
        );
        let (endpoint, server) = spawn_server(app).await;
        let mut provider_request = request(endpoint, None);
        provider_request.messages = vec![
            ProviderMessage::assistant(
                None,
                vec![ProviderToolCall {
                    id: "call_weather".to_owned(),
                    name: "get_weather".to_owned(),
                    arguments_json: r#"{"city":"Paris"}"#.to_owned(),
                }],
            ),
            ProviderMessage::tool("call_weather", r#"{"temperature":20}"#),
        ];
        provider_request.tools = vec![tool_definition("get_weather")];
        let (event_tx, _event_rx) = mpsc::channel(4);
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let turn = OpenAiCompatibleProvider::new()
            .stream_chat(provider_request, event_tx, cancel_rx)
            .await
            .unwrap();

        assert_eq!(turn.finish, ProviderFinish::Stop);
        let body = seen_rx.await.unwrap();
        assert_eq!(
            body["messages"],
            json!([
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_weather",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": r#"{"city":"Paris"}"#
                        }
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_weather",
                    "content": r#"{"temperature":20}"#
                }
            ])
        );
        assert_eq!(
            body["tools"],
            json!([{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "get_weather description",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    },
                    "strict": true
                }
            }])
        );
        assert_eq!(body["tool_choice"], "auto");
        abort_server(server).await;
    }

    #[tokio::test]
    async fn assembles_interleaved_fragmented_tool_calls() {
        let stream = sse_stream(
            &[
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"call_weather","type":"function",
                    "function":{"name":"get_weather","arguments":"{\"city\":\""}
                }]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":1,"id":"call_search","type":"function",
                    "function":{"name":"search","arguments":"{\"q\":\"Rust"}
                }]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"function":{"arguments":"Paris\"}"}
                }]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":1,"function":{"arguments":"\"}"}
                }]},"finish_reason":"tool_calls"}]}),
                json!({"choices":[],"usage":{
                    "prompt_tokens":10,"completion_tokens":4,"total_tokens":14
                }}),
            ],
            true,
        );

        let turn = consume_sse(&stream, &["get_weather", "search"])
            .await
            .unwrap();

        assert_eq!(
            turn,
            ProviderTurn {
                usage: ProviderUsage {
                    prompt_tokens: 10,
                    completion_tokens: 4,
                    total_tokens: 14,
                    cost: None,
                },
                finish: ProviderFinish::ToolCalls(vec![
                    ProviderToolCall {
                        id: "call_weather".to_owned(),
                        name: "get_weather".to_owned(),
                        arguments_json: r#"{"city":"Paris"}"#.to_owned(),
                    },
                    ProviderToolCall {
                        id: "call_search".to_owned(),
                        name: "search".to_owned(),
                        arguments_json: r#"{"q":"Rust"}"#.to_owned(),
                    },
                ]),
            }
        );
    }

    #[tokio::test]
    async fn preserves_duplicate_object_keys_in_raw_tool_arguments() {
        let stream = sse_stream(
            &[
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"call_raw","type":"function",
                    "function":{"name":"known","arguments":"{\"value\":1,"}
                }]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"function":{"arguments":"\"value\":2}"}
                }]},"finish_reason":"tool_calls"}]}),
                json!({"choices":[],"usage":{
                    "prompt_tokens":1,"completion_tokens":1,"total_tokens":2
                }}),
            ],
            true,
        );

        let turn = consume_sse(&stream, &["known"]).await.unwrap();
        let ProviderFinish::ToolCalls(calls) = turn.finish else {
            panic!("expected tool calls");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments_json, r#"{"value":1,"value":2}"#);
    }

    #[tokio::test]
    async fn eof_is_valid_only_after_usage_and_an_explicit_finish() {
        let complete = sse_stream(
            &[
                json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
                json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
            ],
            false,
        );
        assert_eq!(
            consume_sse(&complete, &[]).await.unwrap().finish,
            ProviderFinish::Stop
        );

        let missing_finish = sse_stream(
            &[
                json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
            ],
            true,
        );
        assert_eq!(
            consume_sse(&missing_finish, &[]).await,
            Err(ProviderError::InvalidResponse)
        );
    }

    #[tokio::test]
    async fn rejects_invalid_tool_call_streams() {
        let usage = json!({"choices":[],"usage":{
            "prompt_tokens":1,"completion_tokens":1,"total_tokens":2
        }});
        let cases = [
            json!({"choices":[{"index":0,"delta":{"tool_calls":[]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "id":"call","type":"function","function":{"name":"known","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":1,"id":"call","type":"function","function":{"name":"known","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","type":"computer","function":{"name":"known","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","type":"function","function":{"name":"unknown","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","type":"function","function":{"name":"known","arguments":"[]"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","type":"function","function":{"name":"known","arguments":"{"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","function":{"name":"known","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"type":"function","function":{"name":"known","arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","type":"function","function":{"arguments":"{}"}
            }]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"call","type":"function","function":{"name":"known","arguments":"{}"}
            }]},"finish_reason":"stop"}]}),
            json!({"choices":[{"index":1,"delta":{},"finish_reason":"stop"}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"unsupported"}]}),
        ];

        for invalid in cases {
            let stream = sse_stream(&[invalid, usage.clone()], true);
            assert_eq!(
                consume_sse(&stream, &["known"]).await,
                Err(ProviderError::InvalidResponse),
                "stream should be rejected: {stream}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_inconsistent_or_duplicate_streamed_call_identity() {
        let usage = json!({"choices":[],"usage":{
            "prompt_tokens":1,"completion_tokens":1,"total_tokens":2
        }});
        let cases = [
            vec![
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"first","type":"function","function":{"name":"known","arguments":"{"}
                }]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"second","function":{"arguments":"}"}
                }]},"finish_reason":"tool_calls"}]}),
            ],
            vec![
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"call","type":"function","function":{"name":"known","arguments":"{"}
                }]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"function":{"name":"other","arguments":"}"}
                }]},"finish_reason":"tool_calls"}]}),
            ],
            vec![json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"same","type":"function","function":{"name":"known","arguments":"{}"}},
                {"index":1,"id":"same","type":"function","function":{"name":"known","arguments":"{}"}}
            ]},"finish_reason":"tool_calls"}]})],
        ];

        for mut chunks in cases {
            chunks.push(usage.clone());
            let stream = sse_stream(&chunks, true);
            assert_eq!(
                consume_sse(&stream, &["known", "other"]).await,
                Err(ProviderError::InvalidResponse)
            );
        }
    }

    #[tokio::test]
    async fn cancellation_is_observed_before_network_io() {
        let provider = OpenAiCompatibleProvider::new();
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (_cancel_tx, cancel_rx) = watch::channel(true);
        let endpoint = Url::parse("http://127.0.0.1:1/v1/chat/completions").unwrap();

        let result = provider
            .stream_chat(request(endpoint, None), event_tx, cancel_rx)
            .await;

        assert_eq!(result, Err(ProviderError::Cancelled));
    }

    #[tokio::test]
    async fn timeout_and_http_failures_are_distinct_and_redacted() {
        let slow = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                sleep(Duration::from_millis(200)).await;
                one_byte_sse_response("data: [DONE]\n\n")
            }),
        );
        let (slow_endpoint, slow_server) = spawn_server(slow).await;
        let provider = OpenAiCompatibleProvider::with_client_and_timeout(
            Client::new(),
            Duration::from_millis(20),
        );
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            provider
                .stream_chat(request(slow_endpoint, None), event_tx, cancel_rx)
                .await,
            Err(ProviderError::Timeout)
        );
        abort_server(slow_server).await;

        let rejected = Router::new().route(
            "/v1/chat/completions",
            post(|| async { (StatusCode::UNAUTHORIZED, "never-log-this-secret") }),
        );
        let (rejected_endpoint, rejected_server) = spawn_server(rejected).await;
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let error = OpenAiCompatibleProvider::new()
            .stream_chat(
                request(rejected_endpoint, Some("never-log-this-secret")),
                event_tx,
                cancel_rx,
            )
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::Authentication);
        assert!(!error.to_string().contains("never-log-this-secret"));
        assert!(!format!("{error:?}").contains("never-log-this-secret"));
        abort_server(rejected_server).await;
    }

    #[tokio::test]
    async fn http_failures_have_safe_actionable_categories() {
        for (status, expected) in [
            (StatusCode::FORBIDDEN, ProviderError::Authentication),
            (StatusCode::TOO_MANY_REQUESTS, ProviderError::RateLimited),
            (StatusCode::BAD_REQUEST, ProviderError::RequestRejected),
            (StatusCode::NOT_FOUND, ProviderError::RequestRejected),
            (StatusCode::SERVICE_UNAVAILABLE, ProviderError::Unavailable),
        ] {
            let rejected = Router::new().route(
                "/v1/chat/completions",
                post(move || async move { (status, "provider details stay private") }),
            );
            let (endpoint, server) = spawn_server(rejected).await;
            let (event_tx, _event_rx) = mpsc::channel(1);
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let error = OpenAiCompatibleProvider::new()
                .stream_chat(
                    request(endpoint, Some("never-log-this-secret")),
                    event_tx,
                    cancel_rx,
                )
                .await
                .unwrap_err();
            assert_eq!(error, expected, "unexpected mapping for {status}");
            assert!(!error.to_string().contains("provider details"));
            assert!(!format!("{error:?}").contains("never-log-this-secret"));
            abort_server(server).await;
        }
    }

    #[tokio::test]
    async fn malformed_or_non_sse_success_responses_are_invalid() {
        let malformed = Router::new().route(
            "/v1/chat/completions",
            post(|| async { one_byte_sse_response("data: {not-json}\n\n") }),
        );
        let (endpoint, server) = spawn_server(malformed).await;
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            OpenAiCompatibleProvider::new()
                .stream_chat(request(endpoint, None), event_tx, cancel_rx)
                .await,
            Err(ProviderError::InvalidResponse)
        );
        abort_server(server).await;

        let stream_error = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                one_byte_sse_response(
                    "data: {\"error\":{\"message\":\"provider details stay private\"}}\n\n",
                )
            }),
        );
        let (endpoint, server) = spawn_server(stream_error).await;
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let error = OpenAiCompatibleProvider::new()
            .stream_chat(request(endpoint, None), event_tx, cancel_rx)
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::StreamFailed);
        assert!(!error.to_string().contains("provider details"));
        assert!(!format!("{error:?}").contains("provider details"));
        abort_server(server).await;

        let missing_usage = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                one_byte_sse_response(concat!(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"text\"},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                ))
            }),
        );
        let (endpoint, server) = spawn_server(missing_usage).await;
        let (event_tx, _event_rx) = mpsc::channel(2);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            OpenAiCompatibleProvider::new()
                .stream_chat(request(endpoint, None), event_tx, cancel_rx)
                .await,
            Err(ProviderError::InvalidResponse)
        );
        abort_server(server).await;

        let json_response = Router::new().route(
            "/v1/chat/completions",
            post(|| async { Json(json!({"ok": true})) }),
        );
        let (endpoint, server) = spawn_server(json_response).await;
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            OpenAiCompatibleProvider::new()
                .stream_chat(request(endpoint, None), event_tx, cancel_rx)
                .await,
            Err(ProviderError::InvalidResponse)
        );
        abort_server(server).await;
    }

    #[test]
    fn request_validation_rejects_invalid_tool_shapes_and_limits() {
        let endpoint = Url::parse("http://127.0.0.1:1/v1/chat/completions").unwrap();
        let mut value = request(endpoint.clone(), None);
        value.tools = vec![tool_definition("known"), tool_definition("known")];
        assert_eq!(
            validate_request(&value),
            Err(ProviderError::InvalidConfiguration)
        );

        let mut value = request(endpoint.clone(), None);
        value.tools = vec![ProviderToolDefinition {
            name: "not valid".to_owned(),
            description: String::new(),
            parameters: json!({"type":"object"}),
            strict: None,
        }];
        assert_eq!(
            validate_request(&value),
            Err(ProviderError::InvalidConfiguration)
        );

        let mut value = request(endpoint.clone(), None);
        value.tools = vec![ProviderToolDefinition {
            name: "known".to_owned(),
            description: String::new(),
            parameters: json!([]),
            strict: None,
        }];
        assert_eq!(
            validate_request(&value),
            Err(ProviderError::InvalidConfiguration)
        );

        let mut value = request(endpoint.clone(), None);
        value.messages = vec![ProviderMessage::assistant(None, Vec::new())];
        assert_eq!(
            validate_request(&value),
            Err(ProviderError::InvalidConfiguration)
        );

        let mut value = request(endpoint.clone(), None);
        value.messages = vec![ProviderMessage::tool("", "result")];
        assert_eq!(
            validate_request(&value),
            Err(ProviderError::InvalidConfiguration)
        );

        let mut value = request(endpoint, None);
        value.messages = vec![ProviderMessage::new("developer", "unsupported")];
        assert_eq!(
            validate_request(&value),
            Err(ProviderError::InvalidConfiguration)
        );
    }

    #[test]
    fn streamed_tool_call_limits_are_enforced_before_allocation_growth() {
        let mut state = StreamState::new(HashSet::from(["known".to_owned()]));
        assert_eq!(
            state.push_tool_call(WireToolCallDelta {
                index: Some(MAX_TOOL_CALLS as u64),
                id: Some("call".to_owned()),
                kind: Some("function".to_owned()),
                function: Some(WireFunctionCallDelta {
                    name: Some("known".to_owned()),
                    arguments: Some("{}".to_owned()),
                }),
            }),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(
            state.push_tool_call(WireToolCallDelta {
                index: Some(0),
                id: Some("x".repeat(MAX_TOOL_CALL_ID_BYTES + 1)),
                kind: Some("function".to_owned()),
                function: None,
            }),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(
            state.push_tool_call(WireToolCallDelta {
                index: Some(0),
                id: Some("call".to_owned()),
                kind: Some("function".to_owned()),
                function: Some(WireFunctionCallDelta {
                    name: Some("x".repeat(MAX_TOOL_NAME_BYTES + 1)),
                    arguments: None,
                }),
            }),
            Err(ProviderError::InvalidResponse)
        );

        let mut state = StreamState::new(HashSet::from(["known".to_owned()]));
        assert_eq!(
            state.push_tool_call(WireToolCallDelta {
                index: Some(0),
                id: Some("call".to_owned()),
                kind: Some("function".to_owned()),
                function: Some(WireFunctionCallDelta {
                    name: Some("known".to_owned()),
                    arguments: Some("x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1)),
                }),
            }),
            Err(ProviderError::InvalidResponse)
        );
    }

    fn request(url: Url, secret: Option<&str>) -> ProviderRequest {
        let base_url = url.join("../").unwrap();
        ProviderRequest {
            provider_id: "openai-api".to_owned(),
            model: "test-model".to_owned(),
            base_url,
            url,
            secret: secret.map(|value| SecretString::from(value.to_owned())),
            reasoning_effort: Some("medium".to_owned()),
            messages: vec![ProviderMessage::new("user", "hello")],
            tools: Vec::new(),
        }
    }

    fn tool_definition(name: &str) -> ProviderToolDefinition {
        ProviderToolDefinition {
            name: name.to_owned(),
            description: format!("{name} description"),
            parameters: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}}
            }),
            strict: Some(true),
        }
    }

    fn sse_stream(chunks: &[Value], done: bool) -> String {
        let mut stream = String::new();
        for chunk in chunks {
            stream.push_str("data: ");
            stream.push_str(&chunk.to_string());
            stream.push_str("\n\n");
        }
        if done {
            stream.push_str("data: [DONE]\n\n");
        }
        stream
    }

    async fn consume_sse(
        value: &str,
        allowed_tools: &[&str],
    ) -> Result<ProviderTurn, ProviderError> {
        let stream = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::copy_from_slice(
            value.as_bytes(),
        ))]);
        let (event_tx, _event_rx) = mpsc::channel(32);
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        consume_stream(
            stream,
            &event_tx,
            &mut cancel_rx,
            Duration::from_secs(1),
            allowed_tools
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        )
        .await
    }

    fn one_byte_sse_response(value: &str) -> Response<Body> {
        let chunks: Vec<_> = value
            .as_bytes()
            .iter()
            .copied()
            .map(|byte| Ok::<_, Infallible>(Bytes::from(vec![byte])))
            .collect();
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .body(Body::from_stream(stream::iter(chunks)))
            .unwrap()
    }

    async fn spawn_server(app: Router) -> (Url, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = Url::parse(&format!("http://{address}/v1/chat/completions")).unwrap();
        (endpoint, task)
    }

    async fn abort_server(task: JoinHandle<()>) {
        task.abort();
        let error = task.await.expect_err("fixture server should be cancelled");
        assert!(
            error.is_cancelled(),
            "fixture server should stop by cancellation"
        );
    }

    #[test]
    fn decoder_supports_multiline_data_comments_and_cr_separators() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push(b": heartbeat\rdata: {\"a\":\rdata: 1}\r\r\n")
            .unwrap();
        assert_eq!(events, vec![b"{\"a\":\n1}".to_vec()]);
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_open_stream() {
        let (request_seen_tx, request_seen_rx) = oneshot::channel();
        let request_seen_tx = Arc::new(std::sync::Mutex::new(Some(request_seen_tx)));
        let app = Router::new().route(
            "/v1/chat/completions",
            get(|| async { StatusCode::METHOD_NOT_ALLOWED }).post({
                let request_seen_tx = request_seen_tx.clone();
                move || {
                    let request_seen_tx = request_seen_tx.clone();
                    async move {
                        if let Some(sender) = request_seen_tx.lock().unwrap().take() {
                            let _ = sender.send(());
                        }
                        let body = Body::from_stream(async_stream::stream! {
                            pending::<()>().await;
                            yield Ok::<Bytes, Infallible>(Bytes::new());
                        });
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(CONTENT_TYPE, "text/event-stream")
                            .body(body)
                            .unwrap()
                    }
                }
            }),
        );
        let (endpoint, server) = spawn_server(app).await;
        let provider = OpenAiCompatibleProvider::new();
        let (event_tx, _event_rx) = mpsc::channel(1);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let operation = tokio::spawn(async move {
            provider
                .stream_chat(request(endpoint, None), event_tx, cancel_rx)
                .await
        });
        request_seen_rx.await.unwrap();
        cancel_tx.send(true).unwrap();
        assert_eq!(operation.await.unwrap(), Err(ProviderError::Cancelled));
        abort_server(server).await;
    }
}
