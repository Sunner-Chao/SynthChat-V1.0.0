use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use subtle::ConstantTimeEq;
use tempfile::Builder as TempBuilder;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};
use uuid::Uuid;

use crate::{
    processes::{
        CODE_RPC_PORT_ENVIRONMENT_NAME, CODE_RPC_TOKEN_ENVIRONMENT_NAME, CodeRpcBootstrap,
        DirectProcessRequest, ProcessExecutionContext, ProcessManager, SupervisedDirectProcess,
        sanitized_environment,
    },
    profiles::{CodeExecutionConfig, CodeExecutionMode},
    tools::{ToolExecutionContext, ToolExecutionError, ToolRegistry, ToolRisk},
    web::WebService,
};

const MAX_CODE_BYTES: usize = 60 * 1024;
const MAX_RPC_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RPC_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_STDERR_BYTES: usize = 10_000;
const PYTHON_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const RPC_ALLOWED_TOOLS: [&str; 7] = [
    "web_search",
    "web_extract",
    "read_file",
    "write_file",
    "search_files",
    "patch",
    "terminal",
];

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PreparedCodeExecution {
    code: String,
    config: CodeExecutionConfig,
    allowed_tools: BTreeSet<String>,
}

impl std::fmt::Debug for PreparedCodeExecution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCodeExecution")
            .field("code", &"[redacted]")
            .field("config", &self.config)
            .field("allowed_tools", &self.allowed_tools)
            .finish()
    }
}

impl PreparedCodeExecution {
    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    pub(crate) fn config(&self) -> &CodeExecutionConfig {
        &self.config
    }

    pub(crate) fn allowed_tools(&self) -> &BTreeSet<String> {
        &self.allowed_tools
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeExecutionOutput {
    pub(crate) raw_result_json: String,
    pub(crate) provider_content: String,
    pub(crate) result_summary: String,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum CodeExecutionError {
    #[error("code execution is unavailable")]
    Unavailable,
    #[error("code execution arguments are invalid")]
    InvalidArguments,
    #[error("code execution could not be started")]
    SpawnFailed,
    #[error("code execution failed")]
    ExecutionFailed,
    #[error("code execution was cancelled")]
    Cancelled,
    #[error("code execution deadline was exceeded")]
    DeadlineExceeded,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCodeArguments {
    code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcRequest {
    token: String,
    tool: String,
    args: JsonValue,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RpcAuditEntry {
    tool: String,
    success: bool,
}

#[derive(Clone, Debug)]
struct PythonInterpreter {
    executable: PathBuf,
}

enum ExecutionStop {
    Exited(i32),
    ScriptTimeout,
}

pub(crate) fn is_available() -> bool {
    python_interpreter().is_some()
}

pub(crate) fn prepare(
    raw_arguments_json: &str,
    config: CodeExecutionConfig,
    available_tools: impl IntoIterator<Item = String>,
) -> Result<PreparedCodeExecution, CodeExecutionError> {
    if raw_arguments_json.len() > MAX_CODE_BYTES + 1024 {
        return Err(CodeExecutionError::InvalidArguments);
    }
    let raw: RawCodeArguments = serde_json::from_str(raw_arguments_json)
        .map_err(|_| CodeExecutionError::InvalidArguments)?;
    if raw.code.trim().is_empty() || raw.code.len() > MAX_CODE_BYTES || raw.code.contains('\0') {
        return Err(CodeExecutionError::InvalidArguments);
    }
    let allowed = RPC_ALLOWED_TOOLS.into_iter().collect::<HashSet<_>>();
    let allowed_tools = available_tools
        .into_iter()
        .filter(|tool| allowed.contains(tool.as_str()))
        .collect();
    Ok(PreparedCodeExecution {
        code: raw.code,
        config,
        allowed_tools,
    })
}

pub(crate) fn description(
    allowed_tools: &BTreeSet<String>,
    mode: CodeExecutionMode,
    workspace_available: bool,
) -> String {
    let mut lines = Vec::new();
    for tool in RPC_ALLOWED_TOOLS {
        if allowed_tools.contains(tool) {
            lines.push(match tool {
                "web_search" => "  web_search(query: str, limit: int = 5) -> dict",
                "web_extract" => "  web_extract(urls: list[str], char_limit: int = None) -> dict",
                "read_file" => "  read_file(path: str, offset: int = 1, limit: int = 500) -> dict",
                "write_file" => "  write_file(path: str, content: str) -> dict",
                "search_files" => "  search_files(pattern: str, target='content', path='.', file_glob=None, limit=50, offset=0, output_mode='content', context=0) -> dict",
                "patch" => "  patch(path=None, old_string=None, new_string=None, replace_all=False, mode='replace', patch=None) -> dict",
                "terminal" => "  terminal(command: str, timeout=None, workdir=None) -> dict (foreground only)",
                _ => continue,
            });
        }
    }
    let tools = if lines.is_empty() {
        "  No Hermes RPC tools are available; Python standard-library processing is still available."
            .to_owned()
    } else {
        lines.join("\n")
    };
    let cwd = match (mode, workspace_available) {
        (CodeExecutionMode::Project, true) => {
            "The script runs with the Run Workspace as its working directory."
        }
        (CodeExecutionMode::Project, false) => {
            "No Run Workspace is bound, so project mode falls back to the private staging directory."
        }
        (CodeExecutionMode::Strict, _) => {
            "The script runs in its private staging directory, not the Run Workspace."
        }
    };
    format!(
        "Run a Python script that can call enabled Hermes tools programmatically. Use it for multi-step filtering, branching, loops, or retries; use normal tool calls for a single action or interactive work.\n\nAvailable through `from hermes_tools import ...`:\n{tools}\n\nLimits are Profile-configured up to 10 minutes and 100 RPC calls; stdout is capped at 50 KB and terminal is foreground-only. {cwd} Print the final result to stdout. The process receives a scrubbed environment, but the approved script still has host filesystem, network, subprocess and native-library authority; this is not an OS or container sandbox."
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute(
    registry: &ToolRegistry,
    context: &ToolExecutionContext<'_>,
    processes: &ProcessManager,
    web: &WebService,
    process_context: &ProcessExecutionContext,
    prepared: &PreparedCodeExecution,
    mut cancellation: watch::Receiver<bool>,
    run_deadline: Instant,
) -> Result<CodeExecutionOutput, CodeExecutionError> {
    let interpreter = python_interpreter().ok_or(CodeExecutionError::Unavailable)?;
    if *cancellation.borrow() {
        return Err(CodeExecutionError::Cancelled);
    }
    context.control().check().map_err(map_control_error)?;

    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| CodeExecutionError::ExecutionFailed)?;
    let port = listener
        .local_addr()
        .map_err(|_| CodeExecutionError::ExecutionFailed)?
        .port();
    let rpc_token = random_rpc_token();
    let staging = TempBuilder::new()
        .prefix("synthchat-code-")
        .tempdir()
        .map_err(|_| CodeExecutionError::ExecutionFailed)?;
    let helper_path = staging.path().join("hermes_tools.py");
    let script_path = staging.path().join("script.py");
    std::fs::write(&helper_path, helper_source(prepared.allowed_tools()))
        .and_then(|()| std::fs::write(&script_path, prepared.code()))
        .map_err(|_| CodeExecutionError::ExecutionFailed)?;

    let cwd = match prepared.config().mode {
        CodeExecutionMode::Project => process_context
            .workspace_root
            .as_deref()
            .unwrap_or_else(|| staging.path()),
        CodeExecutionMode::Strict => staging.path(),
    };
    let started = Instant::now();
    let bootstrap = CodeRpcBootstrap::new(port, rpc_token.clone())
        .map_err(|_| CodeExecutionError::SpawnFailed)?;
    let request = DirectProcessRequest::new(
        &interpreter.executable,
        cwd,
        [script_path.as_os_str().to_owned()],
        code_environment(staging.path()),
        Some(bootstrap),
    )
    .map_err(|_| CodeExecutionError::SpawnFailed)?;
    let mut process = SupervisedDirectProcess::spawn(request)
        .await
        .map_err(|_| CodeExecutionError::SpawnFailed)?;

    let script_deadline = started
        .checked_add(Duration::from_secs(prepared.config().timeout_seconds))
        .unwrap_or(run_deadline);
    let inner_deadline = script_deadline.min(run_deadline);
    let call_count = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(Mutex::new(Vec::new()));
    let mut rpc = Box::pin(serve_rpc(
        listener,
        rpc_token,
        prepared.allowed_tools(),
        prepared.config().max_tool_calls,
        registry,
        context,
        processes,
        web,
        process_context,
        cancellation.clone(),
        inner_deadline,
        call_count.clone(),
        audit.clone(),
    ));
    let mut rpc_finished = false;

    let stop = loop {
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                if changed.is_err() || *cancellation.borrow() {
                    let _ = process.terminate().await;
                    return Err(CodeExecutionError::Cancelled);
                }
            }
            _ = tokio::time::sleep_until(run_deadline.into()) => {
                let _ = process.terminate().await;
                return Err(CodeExecutionError::DeadlineExceeded);
            }
            _ = tokio::time::sleep_until(script_deadline.into()) => {
                let _ = process.terminate().await;
                break ExecutionStop::ScriptTimeout;
            }
            status = process.wait() => {
                let status = status.map_err(|_| CodeExecutionError::ExecutionFailed)?;
                break ExecutionStop::Exited(exit_code(status));
            }
            result = &mut rpc, if !rpc_finished => {
                rpc_finished = true;
                if result.is_err() {
                    tracing::warn!("execute_code RPC listener stopped before the child process");
                }
            }
        }
    };
    drop(rpc);

    let secrets = context
        .profiles()
        .secret_redaction_snapshots(context.profile_id())
        .map_err(|_| CodeExecutionError::ExecutionFailed)?;
    let redactor = |value: &str| redact_output(value, &secrets);
    let captured = process
        .captured_output(&redactor, 50_000, MAX_STDERR_BYTES)
        .map_err(|_| CodeExecutionError::ExecutionFailed)?;
    let stdout = captured.stdout;
    let stderr = captured.stderr;
    let tool_calls_made = call_count.load(Ordering::Acquire);
    let duration_seconds = started.elapsed().as_secs_f64();
    let (status, output, error, result_summary) = match stop {
        ExecutionStop::Exited(0) => (
            "success",
            stdout.text,
            JsonValue::Null,
            format!("Python execution completed with {tool_calls_made} tool calls"),
        ),
        ExecutionStop::Exited(exit_code) => {
            let output = append_stderr(stdout.text, &stderr.text);
            (
                "error",
                output,
                JsonValue::String(format!("Script exited with code {exit_code}")),
                format!("Python execution failed with exit code {exit_code}"),
            )
        }
        ExecutionStop::ScriptTimeout => {
            let message = format!(
                "Script timed out after {} seconds and was killed.",
                prepared.config().timeout_seconds
            );
            (
                "timeout",
                append_message(stdout.text, &message),
                JsonValue::String(message),
                "Python execution timed out".to_owned(),
            )
        }
    };
    let provider_value = json!({
        "status": status,
        "output": output,
        "error": error,
        "tool_calls_made": tool_calls_made,
        "duration_seconds": (duration_seconds * 1000.0).round() / 1000.0,
    });
    let provider_content =
        serde_json::to_string(&provider_value).map_err(|_| CodeExecutionError::ExecutionFailed)?;
    let audit = audit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let mut raw_value = provider_value;
    raw_value
        .as_object_mut()
        .expect("the code execution result is an object")
        .insert(
            "tool_calls".to_owned(),
            serde_json::to_value(audit).map_err(|_| CodeExecutionError::ExecutionFailed)?,
        );
    let raw_result_json =
        serde_json::to_string(&raw_value).map_err(|_| CodeExecutionError::ExecutionFailed)?;
    if raw_result_json.len() > MAX_RPC_RESPONSE_BYTES {
        return Err(CodeExecutionError::ExecutionFailed);
    }
    Ok(CodeExecutionOutput {
        raw_result_json,
        provider_content,
        result_summary,
    })
}

#[allow(clippy::too_many_arguments)]
async fn serve_rpc(
    listener: TcpListener,
    token: String,
    allowed_tools: &BTreeSet<String>,
    max_tool_calls: usize,
    registry: &ToolRegistry,
    context: &ToolExecutionContext<'_>,
    processes: &ProcessManager,
    web: &WebService,
    process_context: &ProcessExecutionContext,
    cancellation: watch::Receiver<bool>,
    deadline: Instant,
    call_count: Arc<AtomicUsize>,
    audit: Arc<Mutex<Vec<RpcAuditEntry>>>,
) -> io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let authenticated = handle_rpc_connection(
            stream,
            &token,
            allowed_tools,
            max_tool_calls,
            registry,
            context,
            processes,
            web,
            process_context,
            cancellation.clone(),
            deadline,
            &call_count,
            &audit,
        )
        .await?;
        if authenticated {
            return Ok(());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_rpc_connection(
    mut stream: TcpStream,
    token: &str,
    allowed_tools: &BTreeSet<String>,
    max_tool_calls: usize,
    registry: &ToolRegistry,
    context: &ToolExecutionContext<'_>,
    processes: &ProcessManager,
    web: &WebService,
    process_context: &ProcessExecutionContext,
    cancellation: watch::Receiver<bool>,
    deadline: Instant,
    call_count: &AtomicUsize,
    audit: &Mutex<Vec<RpcAuditEntry>>,
) -> io::Result<bool> {
    let mut pending = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut authenticated = false;
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(authenticated);
        }
        pending.extend_from_slice(&chunk[..read]);
        if pending.len() > MAX_RPC_REQUEST_BYTES {
            write_rpc_error(&mut stream, "RPC request exceeds the fixed limit").await?;
            return Ok(authenticated);
        }
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline).collect::<Vec<_>>();
            if line.len() <= 1 {
                continue;
            }
            let request = match serde_json::from_slice::<RpcRequest>(&line[..line.len() - 1]) {
                Ok(request) => request,
                Err(_) => {
                    write_rpc_error(&mut stream, "RPC request is invalid").await?;
                    continue;
                }
            };
            if request.token.as_bytes().ct_eq(token.as_bytes()).unwrap_u8() != 1 {
                write_rpc_error(&mut stream, "RPC request is unauthorized").await?;
                return Ok(false);
            }
            authenticated = true;
            let response = dispatch_rpc(
                request,
                allowed_tools,
                max_tool_calls,
                registry,
                context,
                processes,
                web,
                process_context,
                cancellation.clone(),
                deadline,
                call_count,
            )
            .await;
            let success = response.get("ok").and_then(JsonValue::as_bool) == Some(true);
            let tool = response
                .get("tool")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown")
                .to_owned();
            audit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(RpcAuditEntry { tool, success });
            write_rpc_response(&mut stream, response).await?;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_rpc(
    request: RpcRequest,
    allowed_tools: &BTreeSet<String>,
    max_tool_calls: usize,
    registry: &ToolRegistry,
    context: &ToolExecutionContext<'_>,
    processes: &ProcessManager,
    web: &WebService,
    process_context: &ProcessExecutionContext,
    cancellation: watch::Receiver<bool>,
    deadline: Instant,
    call_count: &AtomicUsize,
) -> JsonValue {
    let tool = request.tool;
    let failure = |message: &'static str| json!({"ok": false, "tool": tool, "error": message});
    if !allowed_tools.contains(&tool) || !request.args.is_object() {
        return failure("Tool is not available in execute_code");
    }
    let current = call_count.fetch_add(1, Ordering::AcqRel);
    if current >= max_tool_calls {
        call_count.store(max_tool_calls, Ordering::Release);
        return failure("Tool call limit reached");
    }
    let rpc_sequence = match u32::try_from(current + 1) {
        Ok(sequence) => sequence,
        Err(_) => return failure("Tool call limit reached"),
    };
    if tool == "terminal" && terminal_rpc_requests_unsupported_mode(&request.args) {
        return failure("execute_code terminal calls must be foreground and non-interactive");
    }
    let raw_arguments_json = match serde_json::to_string(&request.args) {
        Ok(value) => value,
        Err(_) => return failure("Tool arguments are invalid"),
    };
    let (run_id, parent_call_id) = match (context.run_id(), context.call_id()) {
        (Some(run_id), Some(call_id)) => (run_id.to_owned(), call_id.to_owned()),
        _ => return failure("Tool execution owner is unavailable"),
    };
    let planned = {
        let sessions = context.sessions().clone();
        let run_id = run_id.clone();
        let parent_call_id = parent_call_id.clone();
        let tool = tool.clone();
        let raw_arguments_json = raw_arguments_json.clone();
        tokio::task::spawn_blocking(move || {
            sessions.plan_code_rpc_invocation(
                &run_id,
                &parent_call_id,
                rpc_sequence,
                &tool,
                &raw_arguments_json,
            )
        })
        .await
    };
    let planned = match planned {
        Ok(Ok(invocation)) => invocation,
        _ => return failure("Tool invocation could not be journaled"),
    };
    let started = {
        let sessions = context.sessions().clone();
        let run_id = run_id.clone();
        let call_id = planned.call_id.clone();
        tokio::task::spawn_blocking(move || sessions.start_tool_invocation(&run_id, &call_id)).await
    };
    let started = match started {
        Ok(Ok(invocation)) => invocation,
        _ => return failure("Tool invocation could not be started"),
    };
    let nested_context = context.for_nested_call(&started.call_id);
    let prepared = match registry.prepare(&nested_context, &tool, &raw_arguments_json) {
        Ok(prepared) => prepared,
        Err(error) => {
            persist_rpc_failure(
                context,
                &run_id,
                &started.call_id,
                started.checkpoint,
                rpc_tool_error(error),
            )
            .await;
            return failure(rpc_tool_error(error));
        }
    };
    let mut nested_process_context = process_context.clone();
    nested_process_context.call_id = started.call_id.clone();
    let result = if ToolRegistry::requires_async_execution(&prepared) {
        Box::pin(registry.execute_prepared_async(
            &nested_context,
            processes,
            web,
            nested_process_context,
            &tool,
            &raw_arguments_json,
            &prepared,
            cancellation,
            deadline,
        ))
        .await
    } else {
        registry.execute_prepared(&nested_context, &tool, &raw_arguments_json, &prepared)
    };
    match result {
        Ok(output) => {
            let result = match serde_json::from_str::<JsonValue>(&output.raw_result_json) {
                Ok(result) => result,
                Err(_) => {
                    persist_rpc_failure(
                        context,
                        &run_id,
                        &started.call_id,
                        started.checkpoint,
                        "Tool result is invalid",
                    )
                    .await;
                    return failure("Tool result is invalid");
                }
            };
            let completed = {
                let sessions = context.sessions().clone();
                let run_id = run_id.clone();
                let call_id = started.call_id.clone();
                let raw_result_json = output.raw_result_json;
                let provider_content = output.provider_content;
                tokio::task::spawn_blocking(move || {
                    sessions.complete_tool_invocation(
                        &run_id,
                        &call_id,
                        started.checkpoint,
                        &raw_result_json,
                        &provider_content,
                    )
                })
                .await
            };
            if !matches!(completed, Ok(Ok(_))) {
                return failure("Tool result could not be journaled");
            }
            json!({"ok": true, "tool": tool, "result": result})
        }
        Err(error) => {
            let message = rpc_tool_error(error);
            persist_rpc_failure(
                context,
                &run_id,
                &started.call_id,
                started.checkpoint,
                message,
            )
            .await;
            failure(message)
        }
    }
}

async fn persist_rpc_failure(
    context: &ToolExecutionContext<'_>,
    run_id: &str,
    call_id: &str,
    checkpoint: u64,
    message: &'static str,
) {
    let sessions = context.sessions().clone();
    let run_id = run_id.to_owned();
    let call_id = call_id.to_owned();
    let raw_error_json = serde_json::to_string(&json!({
        "code": "code_rpc_tool_failed",
        "message": message,
    }))
    .expect("the private RPC failure is JSON serializable");
    let provider_content = serde_json::to_string(&json!({
        "ok": false,
        "error": {"code": "code_rpc_tool_failed"},
    }))
    .expect("the private RPC provider failure is JSON serializable");
    let _ = tokio::task::spawn_blocking(move || {
        sessions.fail_tool_invocation(
            &run_id,
            &call_id,
            checkpoint,
            &raw_error_json,
            &provider_content,
        )
    })
    .await;
}

fn terminal_rpc_requests_unsupported_mode(arguments: &JsonValue) -> bool {
    let Some(arguments) = arguments.as_object() else {
        return true;
    };
    arguments.get("background").and_then(JsonValue::as_bool) == Some(true)
        || arguments.get("pty").and_then(JsonValue::as_bool) == Some(true)
        || arguments
            .get("notify_on_complete")
            .and_then(JsonValue::as_bool)
            == Some(true)
        || arguments
            .get("watch_patterns")
            .and_then(JsonValue::as_array)
            .is_some_and(|patterns| !patterns.is_empty())
}

fn rpc_tool_error(error: ToolExecutionError) -> &'static str {
    match error {
        ToolExecutionError::Unavailable => "Tool is unavailable",
        ToolExecutionError::InvalidArguments => "Tool arguments are invalid",
        ToolExecutionError::ExecutionFailed => "Tool execution failed",
        ToolExecutionError::InvalidResult => "Tool result is invalid",
        ToolExecutionError::Cancelled => "Tool execution was cancelled",
        ToolExecutionError::DeadlineExceeded => "Tool execution deadline was exceeded",
        ToolExecutionError::ApprovalRequired => "Tool approval is required",
    }
}

async fn write_rpc_error(stream: &mut TcpStream, message: &'static str) -> io::Result<()> {
    write_rpc_response(
        stream,
        json!({"ok": false, "tool": "unknown", "error": message}),
    )
    .await
}

async fn write_rpc_response(stream: &mut TcpStream, value: JsonValue) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(&value).map_err(io::Error::other)?;
    if encoded.len() > MAX_RPC_RESPONSE_BYTES {
        encoded = serde_json::to_vec(&json!({
            "ok": false,
            "tool": "unknown",
            "error": "RPC response exceeds the fixed limit"
        }))
        .map_err(io::Error::other)?;
    }
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await
}

fn helper_source(allowed_tools: &BTreeSet<String>) -> String {
    let port_environment = serde_json::to_string(CODE_RPC_PORT_ENVIRONMENT_NAME)
        .expect("the RPC port environment name is JSON serializable");
    let token_environment = serde_json::to_string(CODE_RPC_TOKEN_ENVIRONMENT_NAME)
        .expect("the RPC token environment name is JSON serializable");
    let mut source = PYTHON_HELPER_HEADER
        .replace("__RPC_PORT_ENVIRONMENT__", &port_environment)
        .replace("__RPC_TOKEN_ENVIRONMENT__", &token_environment);
    for tool in RPC_ALLOWED_TOOLS {
        if allowed_tools.contains(tool) {
            source.push_str(tool_stub(tool));
        }
    }
    source
}

const PYTHON_HELPER_HEADER: &str = r#""""Auto-generated SynthChat Hermes tool RPC stubs."""
import json
import os
import shlex
import socket
import threading
import time

_RPC_PORT = int(os.environ[__RPC_PORT_ENVIRONMENT__])
_RPC_TOKEN = os.environ[__RPC_TOKEN_ENVIRONMENT__]
_socket = None
_lock = threading.Lock()

def _connect():
    global _socket
    if _socket is None:
        _socket = socket.create_connection(("127.0.0.1", _RPC_PORT), timeout=300)
        _socket.settimeout(300)
    return _socket

def _call(tool, args):
    payload = {"token": _RPC_TOKEN, "tool": tool,
               "args": {key: value for key, value in args.items() if value is not None}}
    request = (json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8")
    with _lock:
        connection = _connect()
        connection.sendall(request)
        response = bytearray()
        while not response.endswith(b"\n"):
            chunk = connection.recv(65536)
            if not chunk:
                raise RuntimeError("SynthChat tool RPC disconnected")
            response.extend(chunk)
            if len(response) > 65536:
                raise RuntimeError("SynthChat tool RPC response exceeded its limit")
    decoded = json.loads(response.decode("utf-8"))
    if not decoded.get("ok"):
        raise RuntimeError(decoded.get("error") or "SynthChat tool RPC failed")
    return decoded.get("result")

def json_parse(text):
    return json.loads(text, strict=False)

def shell_quote(value):
    return shlex.quote(value)

def retry(fn, max_attempts=3, delay=2):
    last_error = None
    for attempt in range(max_attempts):
        try:
            return fn()
        except Exception as error:
            last_error = error
            if attempt < max_attempts - 1:
                time.sleep(delay * (2 ** attempt))
    raise last_error

"#;

fn tool_stub(tool: &str) -> &'static str {
    match tool {
        "web_search" => {
            r#"def web_search(query, limit=5):
    return _call("web_search", {"query": query, "limit": limit})

"#
        }
        "web_extract" => {
            r#"def web_extract(urls, char_limit=None):
    return _call("web_extract", {"urls": urls, "char_limit": char_limit})

"#
        }
        "read_file" => {
            r#"def read_file(path, offset=1, limit=500):
    return _call("read_file", {"path": path, "offset": offset, "limit": limit})

"#
        }
        "write_file" => {
            r#"def write_file(path, content):
    return _call("write_file", {"path": path, "content": content})

"#
        }
        "search_files" => {
            r#"def search_files(pattern, target="content", path=".", file_glob=None, limit=50, offset=0, output_mode="content", context=0):
    return _call("search_files", {"pattern": pattern, "target": target, "path": path, "file_glob": file_glob, "limit": limit, "offset": offset, "output_mode": output_mode, "context": context})

"#
        }
        "patch" => {
            r#"def patch(path=None, old_string=None, new_string=None, replace_all=False, mode="replace", patch=None):
    return _call("patch", {"path": path, "old_string": old_string, "new_string": new_string, "replace_all": replace_all, "mode": mode, "patch": patch})

"#
        }
        "terminal" => {
            r#"def terminal(command, timeout=None, workdir=None):
    return _call("terminal", {"command": command, "timeout": timeout, "workdir": workdir})

"#
        }
        _ => "",
    }
}

fn code_environment(staging: &Path) -> Vec<(OsString, OsString)> {
    let mut environment = sanitized_environment();
    environment.retain(|(name, _)| {
        !matches!(
            name.to_string_lossy().to_ascii_uppercase().as_str(),
            "PYTHONPATH" | "PYTHONSTARTUP" | "PYTHONINSPECT" | "PYTHONBREAKPOINT"
        )
    });
    environment.extend([
        ("PYTHONPATH".into(), staging.as_os_str().to_owned()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PYTHONIOENCODING".into(), "utf-8".into()),
        ("PYTHONUTF8".into(), "1".into()),
    ]);
    environment
}

fn random_rpc_token() -> String {
    format!(
        "{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn python_interpreter() -> Option<&'static PythonInterpreter> {
    static INTERPRETER: OnceLock<Option<PythonInterpreter>> = OnceLock::new();
    INTERPRETER.get_or_init(detect_python).as_ref()
}

fn detect_python() -> Option<PythonInterpreter> {
    if let Some(explicit) = std::env::var_os("SYNTHCHAT_CODE_EXECUTION_PYTHON") {
        let explicit = PathBuf::from(explicit);
        if explicit.is_absolute()
            && let Some(executable) = usable_executable(&explicit)
        {
            return Some(PythonInterpreter { executable });
        }
    }
    for root_name in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
        if let Some(root) = std::env::var_os(root_name) {
            let root = PathBuf::from(root);
            for relative in python_relative_candidates() {
                if let Some(executable) = usable_executable(&root.join(relative)) {
                    return Some(PythonInterpreter { executable });
                }
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    let mut fallbacks = Vec::new();
    for directory in std::env::split_paths(&path) {
        for name in python_path_candidates() {
            if let Some(executable) = usable_executable(&directory.join(name)) {
                if executable
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("windowsapps")
                {
                    fallbacks.push(executable);
                } else {
                    return Some(PythonInterpreter { executable });
                }
            }
        }
    }
    fallbacks
        .into_iter()
        .next()
        .map(|executable| PythonInterpreter { executable })
}

#[cfg(target_os = "windows")]
fn python_relative_candidates() -> &'static [&'static str] {
    &["Scripts/python.exe", "python.exe"]
}

#[cfg(not(target_os = "windows"))]
fn python_relative_candidates() -> &'static [&'static str] {
    &["bin/python3", "bin/python"]
}

#[cfg(target_os = "windows")]
fn python_path_candidates() -> &'static [&'static str] {
    &["python.exe", "python3.exe"]
}

#[cfg(not(target_os = "windows"))]
fn python_path_candidates() -> &'static [&'static str] {
    &["python3", "python"]
}

fn usable_executable(path: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(path).ok()?;
    let metadata = std::fs::metadata(&canonical).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    if canonical
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains("windowsapps")
        || !probe_supported_python(&canonical)
    {
        return None;
    }
    Some(canonical)
}

fn probe_supported_python(executable: &Path) -> bool {
    let mut command = StdCommand::new(executable);
    command
        .arg("--version")
        .env_clear()
        .envs(sanitized_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + PYTHON_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let Ok(output) = child.wait_with_output() else {
                    return false;
                };
                if !status.success() {
                    return false;
                }
                let mut version = output.stdout;
                version.extend_from_slice(&output.stderr);
                return std::str::from_utf8(&version)
                    .ok()
                    .and_then(parse_python_version)
                    .is_some_and(|(major, minor)| major == 3 && minor >= 8);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn parse_python_version(value: &str) -> Option<(u32, u32)> {
    let version = value.trim().strip_prefix("Python ")?;
    let mut segments = version.split('.');
    let major = segments.next()?.parse().ok()?;
    let minor = segments.next()?.parse().ok()?;
    Some((major, minor))
}

fn redact_output(value: &str, secrets: &[SecretString]) -> String {
    static TOKEN_PATTERN: OnceLock<Regex> = OnceLock::new();
    static BEARER_PATTERN: OnceLock<Regex> = OnceLock::new();

    let mut redacted = value.to_owned();
    for secret in secrets {
        let secret = secret.expose_secret();
        if secret.len() >= 4 {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
    }
    let redacted = TOKEN_PATTERN
        .get_or_init(|| {
            Regex::new(r"(?i)\b(?:sk|ghp|github_pat|xox[baprs]|AIza)[-_A-Za-z0-9]{12,}\b")
                .expect("the code output token redaction regex is valid")
        })
        .replace_all(&redacted, "[REDACTED]");
    BEARER_PATTERN
        .get_or_init(|| {
            Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{12,}")
                .expect("the code output bearer redaction regex is valid")
        })
        .replace_all(&redacted, "$1[REDACTED]")
        .into_owned()
}

fn append_stderr(mut stdout: String, stderr: &str) -> String {
    if stderr.is_empty() {
        return stdout;
    }
    if !stdout.is_empty() && !stdout.ends_with('\n') {
        stdout.push('\n');
    }
    stdout.push_str("--- stderr ---\n");
    stdout.push_str(stderr);
    stdout
}

fn append_message(mut output: String, message: &str) -> String {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(message);
    output
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn map_control_error(error: crate::tools::ToolExecutionControlError) -> CodeExecutionError {
    match error {
        crate::tools::ToolExecutionControlError::Cancelled => CodeExecutionError::Cancelled,
        crate::tools::ToolExecutionControlError::DeadlineExceeded => {
            CodeExecutionError::DeadlineExceeded
        }
    }
}

pub(crate) fn map_tool_error(error: CodeExecutionError) -> ToolExecutionError {
    match error {
        CodeExecutionError::Unavailable => ToolExecutionError::Unavailable,
        CodeExecutionError::InvalidArguments => ToolExecutionError::InvalidArguments,
        CodeExecutionError::Cancelled => ToolExecutionError::Cancelled,
        CodeExecutionError::DeadlineExceeded => ToolExecutionError::DeadlineExceeded,
        CodeExecutionError::SpawnFailed | CodeExecutionError::ExecutionFailed => {
            ToolExecutionError::ExecutionFailed
        }
    }
}

pub(crate) fn input_summary() -> String {
    "Execute Python programmatic tool script".to_owned()
}

pub(crate) fn approval_summary(prepared: &PreparedCodeExecution, preview: &str) -> String {
    let lines = prepared.code().lines().count().max(1);
    format!(
        "Execute host-authority Python script ({lines} lines, {} bytes): {preview}",
        prepared.code().len()
    )
}

pub(crate) fn risk() -> ToolRisk {
    ToolRisk::ApprovalRequired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mode: CodeExecutionMode) -> CodeExecutionConfig {
        CodeExecutionConfig {
            mode,
            timeout_seconds: 300,
            max_tool_calls: 50,
        }
    }

    #[test]
    fn arguments_are_strict_bounded_and_never_debug_code() {
        let prepared = prepare(
            r#"{"code":"print('private')"}"#,
            config(CodeExecutionMode::Project),
            ["web_search".to_owned(), "memory".to_owned()],
        )
        .unwrap();
        assert_eq!(
            prepared.allowed_tools().iter().collect::<Vec<_>>(),
            ["web_search"]
        );
        assert!(!format!("{prepared:?}").contains("private"));
        for invalid in [
            "{}",
            r#"{"code":""}"#,
            r#"{"code":"print(1)","extra":true}"#,
            r#"{"code":null}"#,
        ] {
            assert_eq!(
                prepare(invalid, config(CodeExecutionMode::Strict), Vec::new()),
                Err(CodeExecutionError::InvalidArguments)
            );
        }
    }

    #[test]
    fn helper_only_exports_the_frozen_dynamic_allowlist() {
        let allowed = BTreeSet::from(["read_file".to_owned(), "terminal".to_owned()]);
        let source = helper_source(&allowed);
        assert!(source.starts_with("\"\"\"Auto-generated"));
        assert!(source.contains("def read_file"));
        assert!(source.contains("def terminal"));
        assert!(!source.contains("def web_search"));
        assert!(!source.contains("def execute_code"));
        assert!(source.contains(CODE_RPC_PORT_ENVIRONMENT_NAME));
        assert!(source.contains(CODE_RPC_TOKEN_ENVIRONMENT_NAME));
        assert!(!source.contains("private-token"));
    }

    #[test]
    fn descriptions_are_mode_and_allowlist_aware_without_sandbox_claims() {
        let allowed = BTreeSet::from(["web_search".to_owned()]);
        let strict = description(&allowed, CodeExecutionMode::Strict, true);
        assert!(strict.contains("web_search"));
        assert!(!strict.contains("read_file(path"));
        assert!(strict.contains("not an OS or container sandbox"));
        let project = description(&allowed, CodeExecutionMode::Project, false);
        assert!(project.contains("falls back to the private staging directory"));
    }

    #[test]
    fn output_redaction_removes_exact_and_common_tokens() {
        let secrets = vec![SecretString::from("exact-private-value".to_owned())];
        let output = redact_output(
            "exact-private-value sk-abcdefghijklmnopqrst bearer abcdefghijklmnop",
            &secrets,
        );
        assert!(!output.contains("exact-private-value"));
        assert!(!output.contains("sk-abcdefghijklmnopqrst"));
        assert!(!output.contains("abcdefghijklmnop"));
    }

    #[test]
    fn python_version_probe_requires_supported_python_three() {
        assert_eq!(parse_python_version("Python 3.8.0\r\n"), Some((3, 8)));
        assert_eq!(parse_python_version("Python 3.13.2\n"), Some((3, 13)));
        assert_eq!(parse_python_version("Python 2.7.18"), Some((2, 7)));
        assert_eq!(parse_python_version("Python 3"), None);
        assert_eq!(parse_python_version("not python"), None);
    }
}
