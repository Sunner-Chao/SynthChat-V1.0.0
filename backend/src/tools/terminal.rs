use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use super::runtime::{ToolRisk, ToolSpec};

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_STDIN_BYTES: usize = 32 * 1024;
const MAX_WORKDIR_BYTES: usize = 1_024;
const MAX_PATH_COMPONENT_BYTES: usize = 255;
const MAX_WATCH_PATTERNS: usize = 16;
const MAX_WATCH_PATTERN_CHARS: usize = 256;
const DEFAULT_FOREGROUND_TIMEOUT_SECONDS: u64 = 180;
const MAX_TERMINAL_TIMEOUT_SECONDS: u64 = 600;
const DEFAULT_LOG_LIMIT: u64 = 200;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum TerminalContractError {
    #[error("tool arguments exceed the bounded input limit")]
    InputTooLarge,
    #[error("tool arguments are invalid")]
    InvalidArguments,
    #[error("terminal workdir is not a portable Workspace-relative path")]
    InvalidWorkdir,
    #[error("async terminal delivery is unavailable")]
    AsyncDeliveryUnavailable,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WorkspaceRelativePath(String);

impl WorkspaceRelativePath {
    /// Lexical validation only. Execution must re-check every component no-follow.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalArguments {
    pub(crate) command: String,
    pub(crate) background: bool,
    pub(crate) timeout_seconds: Option<u64>,
    pub(crate) workdir: Option<WorkspaceRelativePath>,
    pub(crate) pty: bool,
    pub(crate) notify_on_complete: bool,
    pub(crate) watch_patterns: Vec<String>,
}

impl TerminalArguments {
    pub(crate) fn risk(&self) -> ToolRisk {
        ToolRisk::ApprovalRequired
    }

    pub(crate) fn foreground_timeout_seconds(&self) -> u64 {
        self.timeout_seconds
            .unwrap_or(DEFAULT_FOREGROUND_TIMEOUT_SECONDS)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProcessAction {
    List,
    Poll,
    Log,
    Wait,
    Kill,
    Write,
    Submit,
    Close,
}

impl ProcessAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Poll => "poll",
            Self::Log => "log",
            Self::Wait => "wait",
            Self::Kill => "kill",
            Self::Write => "write",
            Self::Submit => "submit",
            Self::Close => "close",
        }
    }

    fn risk(self) -> ToolRisk {
        match self {
            Self::List | Self::Poll | Self::Log | Self::Wait => ToolRisk::ReadOnly,
            Self::Kill | Self::Write | Self::Submit | Self::Close => ToolRisk::ApprovalRequired,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProcessArguments {
    List,
    Poll {
        session_id: String,
    },
    Log {
        session_id: String,
        offset: u64,
        limit: u64,
    },
    Wait {
        session_id: String,
        timeout_seconds: Option<u64>,
    },
    Kill {
        session_id: String,
    },
    Write {
        session_id: String,
        data: String,
    },
    Submit {
        session_id: String,
        data: String,
    },
    Close {
        session_id: String,
    },
}

impl ProcessArguments {
    pub(crate) fn action(&self) -> ProcessAction {
        match self {
            Self::List => ProcessAction::List,
            Self::Poll { .. } => ProcessAction::Poll,
            Self::Log { .. } => ProcessAction::Log,
            Self::Wait { .. } => ProcessAction::Wait,
            Self::Kill { .. } => ProcessAction::Kill,
            Self::Write { .. } => ProcessAction::Write,
            Self::Submit { .. } => ProcessAction::Submit,
            Self::Close { .. } => ProcessAction::Close,
        }
    }

    pub(crate) fn risk(&self) -> ToolRisk {
        self.action().risk()
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TerminalForegroundResult {
    pub(crate) output: String,
    pub(crate) exit_code: i32,
    pub(crate) error: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackgroundProcessStatus {
    Running,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TerminalBackgroundResult {
    pub(crate) output: String,
    pub(crate) session_id: String,
    pub(crate) pid: Option<u32>,
    pub(crate) status: BackgroundProcessStatus,
    pub(crate) exit_code: i32,
    pub(crate) error: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessSummary {
    pub(crate) session_id: String,
    pub(crate) command_preview: String,
    pub(crate) status: BackgroundProcessStatus,
    pub(crate) pid: Option<u32>,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessListResult {
    pub(crate) processes: Vec<ProcessSummary>,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessStatusResult {
    pub(crate) session_id: String,
    pub(crate) status: String,
    pub(crate) output: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) error: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessLogResult {
    pub(crate) session_id: String,
    pub(crate) offset: u64,
    pub(crate) lines: Vec<String>,
    pub(crate) next_offset: Option<u64>,
    pub(crate) total_lines: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTerminalArguments {
    command: String,
    #[serde(default)]
    background: bool,
    timeout: Option<u64>,
    workdir: Option<String>,
    #[serde(default)]
    pty: bool,
    #[serde(default)]
    notify_on_complete: bool,
    #[serde(default)]
    watch_patterns: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProcessArguments {
    action: ProcessAction,
    session_id: Option<String>,
    data: Option<String>,
    timeout: Option<u64>,
    offset: Option<u64>,
    limit: Option<u64>,
}

pub(crate) fn parse_terminal_arguments(
    raw_arguments_json: &str,
    async_tool_delivery: bool,
) -> Result<TerminalArguments, TerminalContractError> {
    let raw: RawTerminalArguments = strict_json_object(raw_arguments_json)?;
    if raw.command.is_empty()
        || raw.command.len() > MAX_COMMAND_BYTES
        || raw.command.contains('\0')
        || raw
            .timeout
            .is_some_and(|timeout| timeout == 0 || timeout > MAX_TERMINAL_TIMEOUT_SECONDS)
    {
        return Err(TerminalContractError::InvalidArguments);
    }

    let workdir = raw.workdir.as_deref().map(parse_workdir).transpose()?;
    validate_watch_patterns(&raw.watch_patterns)?;
    let requested_async_delivery = raw.notify_on_complete || !raw.watch_patterns.is_empty();
    if requested_async_delivery && !async_tool_delivery {
        return Err(TerminalContractError::AsyncDeliveryUnavailable);
    }
    if requested_async_delivery && !raw.background {
        return Err(TerminalContractError::InvalidArguments);
    }
    if raw.notify_on_complete && !raw.watch_patterns.is_empty() {
        return Err(TerminalContractError::InvalidArguments);
    }

    Ok(TerminalArguments {
        command: raw.command,
        background: raw.background,
        timeout_seconds: raw.timeout,
        workdir,
        pty: raw.pty,
        notify_on_complete: raw.notify_on_complete,
        watch_patterns: raw.watch_patterns,
    })
}

pub(crate) fn parse_process_arguments(
    raw_arguments_json: &str,
) -> Result<ProcessArguments, TerminalContractError> {
    let raw: RawProcessArguments = strict_json_object(raw_arguments_json)?;
    if raw.timeout == Some(0) || raw.limit == Some(0) {
        return Err(TerminalContractError::InvalidArguments);
    }
    let session_id = raw
        .session_id
        .as_deref()
        .map(validate_session_id)
        .transpose()?;
    if raw
        .data
        .as_ref()
        .is_some_and(|data| data.len() > MAX_STDIN_BYTES || data.contains('\0'))
    {
        return Err(TerminalContractError::InvalidArguments);
    }

    match raw.action {
        ProcessAction::List => {
            require_absent(&session_id, &raw.data, raw.timeout, raw.offset, raw.limit)?;
            Ok(ProcessArguments::List)
        }
        ProcessAction::Poll => {
            require_absent(
                &None::<String>,
                &raw.data,
                raw.timeout,
                raw.offset,
                raw.limit,
            )?;
            Ok(ProcessArguments::Poll {
                session_id: required_session_id(session_id)?,
            })
        }
        ProcessAction::Log => {
            require_absent(&None::<String>, &raw.data, raw.timeout, None, None)?;
            Ok(ProcessArguments::Log {
                session_id: required_session_id(session_id)?,
                offset: raw.offset.unwrap_or(0),
                limit: raw.limit.unwrap_or(DEFAULT_LOG_LIMIT),
            })
        }
        ProcessAction::Wait => {
            require_absent(&None::<String>, &raw.data, None, raw.offset, raw.limit)?;
            Ok(ProcessArguments::Wait {
                session_id: required_session_id(session_id)?,
                timeout_seconds: raw.timeout,
            })
        }
        ProcessAction::Kill => {
            require_absent(
                &None::<String>,
                &raw.data,
                raw.timeout,
                raw.offset,
                raw.limit,
            )?;
            Ok(ProcessArguments::Kill {
                session_id: required_session_id(session_id)?,
            })
        }
        ProcessAction::Write => {
            require_absent(
                &None::<String>,
                &None::<String>,
                raw.timeout,
                raw.offset,
                raw.limit,
            )?;
            Ok(ProcessArguments::Write {
                session_id: required_session_id(session_id)?,
                data: raw.data.ok_or(TerminalContractError::InvalidArguments)?,
            })
        }
        ProcessAction::Submit => {
            require_absent(
                &None::<String>,
                &None::<String>,
                raw.timeout,
                raw.offset,
                raw.limit,
            )?;
            Ok(ProcessArguments::Submit {
                session_id: required_session_id(session_id)?,
                data: raw.data.ok_or(TerminalContractError::InvalidArguments)?,
            })
        }
        ProcessAction::Close => {
            require_absent(
                &None::<String>,
                &raw.data,
                raw.timeout,
                raw.offset,
                raw.limit,
            )?;
            Ok(ProcessArguments::Close {
                session_id: required_session_id(session_id)?,
            })
        }
    }
}

pub(crate) fn terminal_spec() -> ToolSpec {
    ToolSpec {
        name: "terminal",
        toolset_id: "terminal",
        description: "Execute a shell command with host-level authority from an optional Workspace-relative initial directory. Every call requires approval.",
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "command": {"type": "string", "minLength": 1, "maxLength": MAX_COMMAND_BYTES},
                "background": {"type": "boolean", "default": false},
                "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_TERMINAL_TIMEOUT_SECONDS},
                "workdir": {"type": "string", "minLength": 1, "maxLength": MAX_WORKDIR_BYTES},
                "pty": {"type": "boolean", "default": false},
                "notify_on_complete": {"type": "boolean", "default": false},
                "watch_patterns": {
                    "type": "array",
                    "maxItems": MAX_WATCH_PATTERNS,
                    "items": {"type": "string", "minLength": 1, "maxLength": MAX_WATCH_PATTERN_CHARS}
                }
            },
            "required": ["command"]
        }),
        risk: ToolRisk::ApprovalRequired,
    }
}

pub(crate) fn process_spec() -> ToolSpec {
    ToolSpec {
        name: "process",
        toolset_id: "terminal",
        description: "Inspect or control background terminal processes owned by the current Profile and Session.",
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "poll", "log", "wait", "kill", "write", "submit", "close"]
                },
                "session_id": {
                    "type": "string",
                    "minLength": 1,
                    "pattern": "^[A-Za-z0-9_-]+$"
                },
                "data": {"type": "string", "maxLength": MAX_STDIN_BYTES},
                "timeout": {"type": "integer", "minimum": 1},
                "offset": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1}
            },
            "required": ["action"]
        }),
        // The runtime must replace this conservative fallback with parsed action risk.
        risk: ToolRisk::ApprovalRequired,
    }
}

pub(crate) fn terminal_input_summary(arguments: &TerminalArguments) -> String {
    let mode = if arguments.background {
        "background"
    } else {
        "foreground"
    };
    format!("Run terminal command ({mode})")
}

pub(crate) fn terminal_approval_summary(
    arguments: &TerminalArguments,
    command_preview: &str,
) -> String {
    let mode = if arguments.background {
        "background"
    } else {
        "foreground"
    };
    format!("Run terminal command ({mode}): {command_preview}")
}

pub(crate) fn process_input_summary(arguments: &ProcessArguments) -> String {
    format!("Process {}", arguments.action().as_str())
}

pub(crate) fn process_approval_summary(
    arguments: &ProcessArguments,
    data_preview: Option<&str>,
) -> String {
    match arguments {
        ProcessArguments::List => "List background processes".to_owned(),
        ProcessArguments::Poll { session_id }
        | ProcessArguments::Log { session_id, .. }
        | ProcessArguments::Wait { session_id, .. } => {
            format!("Process {} {session_id}", arguments.action().as_str())
        }
        ProcessArguments::Kill { session_id } => format!("Kill process {session_id}"),
        ProcessArguments::Write { session_id, .. }
        | ProcessArguments::Submit { session_id, .. } => format!(
            "Process {} {session_id}: {}",
            arguments.action().as_str(),
            data_preview.unwrap_or("[REDACTED]")
        ),
        ProcessArguments::Close { session_id } => format!("Close stdin for process {session_id}"),
    }
}

fn strict_json_object<T: DeserializeOwned>(raw: &str) -> Result<T, TerminalContractError> {
    if raw.is_empty() {
        return Err(TerminalContractError::InvalidArguments);
    }
    if raw.len() > MAX_ARGUMENT_BYTES {
        return Err(TerminalContractError::InputTooLarge);
    }
    let value: JsonValue =
        serde_json::from_str(raw).map_err(|_| TerminalContractError::InvalidArguments)?;
    let object = value
        .as_object()
        .ok_or(TerminalContractError::InvalidArguments)?;
    if object.values().any(JsonValue::is_null) {
        return Err(TerminalContractError::InvalidArguments);
    }
    serde_json::from_value(value).map_err(|_| TerminalContractError::InvalidArguments)
}

fn parse_workdir(raw: &str) -> Result<WorkspaceRelativePath, TerminalContractError> {
    if raw.is_empty()
        || raw.len() > MAX_WORKDIR_BYTES
        || raw.starts_with(['/', '\\'])
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return Err(TerminalContractError::InvalidWorkdir);
    }
    let mut components = Vec::new();
    for component in raw.split('/') {
        if component.is_empty() || component == ".." {
            return Err(TerminalContractError::InvalidWorkdir);
        }
        if component == "." {
            continue;
        }
        if !portable_component(component) {
            return Err(TerminalContractError::InvalidWorkdir);
        }
        components.push(component);
    }
    if components.is_empty() {
        Ok(WorkspaceRelativePath(".".to_owned()))
    } else {
        Ok(WorkspaceRelativePath(components.join("/")))
    }
}

fn portable_component(component: &str) -> bool {
    if component.len() > MAX_PATH_COMPONENT_BYTES
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return false;
    }
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_lowercase();
    if matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) {
        return false;
    }
    let bytes = stem.as_bytes();
    !(bytes.len() == 4 && matches!(&bytes[..3], b"com" | b"lpt") && matches!(bytes[3], b'1'..=b'9'))
}

fn validate_watch_patterns(patterns: &[String]) -> Result<(), TerminalContractError> {
    if patterns.len() > MAX_WATCH_PATTERNS
        || patterns.iter().any(|pattern| {
            pattern.is_empty()
                || pattern.chars().count() > MAX_WATCH_PATTERN_CHARS
                || pattern.contains('\0')
        })
    {
        Err(TerminalContractError::InvalidArguments)
    } else {
        Ok(())
    }
}

fn validate_session_id(value: &str) -> Result<String, TerminalContractError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Err(TerminalContractError::InvalidArguments)
    } else {
        Ok(value.to_owned())
    }
}

fn required_session_id(value: Option<String>) -> Result<String, TerminalContractError> {
    value.ok_or(TerminalContractError::InvalidArguments)
}

fn require_absent<T, U>(
    session_id: &Option<T>,
    data: &Option<U>,
    timeout: Option<u64>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<(), TerminalContractError> {
    if session_id.is_some()
        || data.is_some()
        || timeout.is_some()
        || offset.is_some()
        || limit.is_some()
    {
        Err(TerminalContractError::InvalidArguments)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn schemas_are_strict_bounded_and_hide_internal_fields() {
        let terminal = terminal_spec();
        assert_eq!(terminal.name, "terminal");
        assert_eq!(terminal.toolset_id, "terminal");
        assert_eq!(terminal.risk, ToolRisk::ApprovalRequired);
        assert_eq!(terminal.input_schema["type"], "object");
        assert_eq!(terminal.input_schema["additionalProperties"], false);
        assert_eq!(terminal.input_schema["required"], json!(["command"]));
        assert_eq!(
            terminal.input_schema["properties"]["command"]["maxLength"],
            MAX_COMMAND_BYTES
        );
        assert_eq!(
            terminal.input_schema["properties"]["timeout"]["maximum"],
            MAX_TERMINAL_TIMEOUT_SECONDS
        );
        assert_eq!(
            terminal.input_schema["properties"]["workdir"]["maxLength"],
            MAX_WORKDIR_BYTES
        );
        assert_eq!(
            terminal.input_schema["properties"]["watch_patterns"]["maxItems"],
            MAX_WATCH_PATTERNS
        );
        let terminal_fields = terminal.input_schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            terminal_fields,
            BTreeSet::from([
                "background",
                "command",
                "notify_on_complete",
                "pty",
                "timeout",
                "watch_patterns",
                "workdir",
            ])
        );
        for internal in ["task_id", "session_id", "force"] {
            assert!(terminal.input_schema["properties"].get(internal).is_none());
        }

        let process = process_spec();
        assert_eq!(process.name, "process");
        assert_eq!(process.toolset_id, "terminal");
        assert_eq!(process.risk, ToolRisk::ApprovalRequired);
        assert_eq!(process.input_schema["additionalProperties"], false);
        assert_eq!(process.input_schema["required"], json!(["action"]));
        assert_eq!(
            process.input_schema["properties"]["action"]["enum"],
            json!([
                "list", "poll", "log", "wait", "kill", "write", "submit", "close"
            ])
        );
        assert_eq!(
            process.input_schema["properties"]["data"]["maxLength"],
            MAX_STDIN_BYTES
        );
        assert!(process.input_schema["properties"].get("task_id").is_none());
        assert!(process.input_schema["properties"].get("force").is_none());
    }

    #[test]
    fn terminal_defaults_and_byte_timeout_bounds_are_strict() {
        let parsed = parse_terminal_arguments(r#"{"command":"echo ok"}"#, false).unwrap();
        assert_eq!(parsed.command, "echo ok");
        assert!(!parsed.background);
        assert_eq!(parsed.timeout_seconds, None);
        assert_eq!(parsed.foreground_timeout_seconds(), 180);
        assert!(parsed.workdir.is_none());
        assert!(!parsed.pty);
        assert!(!parsed.notify_on_complete);
        assert!(parsed.watch_patterns.is_empty());
        assert_eq!(parsed.risk(), ToolRisk::ApprovalRequired);
        assert_eq!(
            terminal_input_summary(&parsed),
            "Run terminal command (foreground)"
        );
        assert_eq!(
            terminal_approval_summary(&parsed, "`echo ok`"),
            "Run terminal command (foreground): `echo ok`"
        );

        for timeout in [1, MAX_TERMINAL_TIMEOUT_SECONDS] {
            let raw = json!({"command": "ok", "timeout": timeout}).to_string();
            assert_eq!(
                parse_terminal_arguments(&raw, false)
                    .unwrap()
                    .timeout_seconds,
                Some(timeout)
            );
        }
        for timeout in [0, MAX_TERMINAL_TIMEOUT_SECONDS + 1] {
            let raw = json!({"command": "ok", "timeout": timeout}).to_string();
            assert_eq!(
                parse_terminal_arguments(&raw, false),
                Err(TerminalContractError::InvalidArguments)
            );
        }

        let command_at_limit = "x".repeat(MAX_COMMAND_BYTES);
        assert!(
            parse_terminal_arguments(&json!({"command": command_at_limit}).to_string(), false)
                .is_ok()
        );
        let oversized_command = "x".repeat(MAX_COMMAND_BYTES + 1);
        assert_eq!(
            parse_terminal_arguments(&json!({"command": oversized_command}).to_string(), false),
            Err(TerminalContractError::InvalidArguments)
        );
        let glyph = "\u{754c}";
        let utf8_command_at_limit = glyph.repeat(MAX_COMMAND_BYTES / glyph.len());
        assert!(
            parse_terminal_arguments(
                &json!({"command": utf8_command_at_limit}).to_string(),
                false
            )
            .is_ok()
        );
        let oversized_utf8_command = format!("{utf8_command_at_limit}{glyph}");
        assert_eq!(
            parse_terminal_arguments(
                &json!({"command": oversized_utf8_command}).to_string(),
                false
            ),
            Err(TerminalContractError::InvalidArguments)
        );
        assert_eq!(
            parse_terminal_arguments(&"x".repeat(MAX_ARGUMENT_BYTES + 1), false),
            Err(TerminalContractError::InputTooLarge)
        );
    }

    #[test]
    fn terminal_rejects_unknown_internal_null_and_malformed_fields() {
        for raw in [
            "",
            "[]",
            r#"{"command":""}"#,
            r#"{"command":null}"#,
            r#"{"command":"ok","timeout":null}"#,
            r#"{"command":"ok","background":"false"}"#,
            r#"{"command":"ok","task_id":"internal"}"#,
            r#"{"command":"ok","session_id":"internal"}"#,
            r#"{"command":"ok","force":true}"#,
            "{",
        ] {
            assert!(
                parse_terminal_arguments(raw, false).is_err(),
                "invalid terminal input accepted: {raw:?}"
            );
        }
    }

    #[test]
    fn workdir_is_lexically_normalized_and_portable() {
        for (raw, expected) in [
            (".", "."),
            ("./.", "."),
            ("./src/./nested", "src/nested"),
            ("src/\u{8d44}\u{6599}", "src/\u{8d44}\u{6599}"),
            ("\u{e9}\u{e9}", "\u{e9}\u{e9}"),
        ] {
            assert_eq!(parse_workdir(raw).unwrap().as_str(), expected);
        }

        let path_at_limit = (0..5)
            .map(|_| "a".repeat(204))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(path_at_limit.len(), MAX_WORKDIR_BYTES);
        assert!(parse_workdir(&path_at_limit).is_ok());
        assert_eq!(
            parse_workdir(&format!("{path_at_limit}a")),
            Err(TerminalContractError::InvalidWorkdir)
        );
        let glyph = "\u{754c}";
        let utf8_path_at_limit = (0..5)
            .map(|_| glyph.repeat(68))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(utf8_path_at_limit.len(), MAX_WORKDIR_BYTES);
        assert!(parse_workdir(&utf8_path_at_limit).is_ok());
        assert_eq!(
            parse_workdir(&format!("{utf8_path_at_limit}{glyph}")),
            Err(TerminalContractError::InvalidWorkdir)
        );

        let oversized_component = "a".repeat(MAX_PATH_COMPONENT_BYTES + 1);
        for raw in [
            "",
            "/absolute",
            r"C:\absolute",
            "C:/absolute",
            "../escape",
            "src/../escape",
            "src//nested",
            "src/trailing.",
            "src/trailing ",
            "src/file:stream",
            "src/CON.txt",
            "src/LPT9",
            "src/bad?",
            "src/control\u{0001}",
            &oversized_component,
        ] {
            assert_eq!(
                parse_workdir(raw),
                Err(TerminalContractError::InvalidWorkdir),
                "unsafe workdir accepted: {raw:?}"
            );
        }
    }

    #[test]
    fn async_delivery_flags_are_rejected_or_validated_without_silent_downgrade() {
        for raw in [
            json!({"command": "job", "background": true, "notify_on_complete": true}),
            json!({"command": "job", "background": true, "watch_patterns": ["ready"]}),
        ] {
            assert_eq!(
                parse_terminal_arguments(&raw.to_string(), false),
                Err(TerminalContractError::AsyncDeliveryUnavailable)
            );
        }
        assert!(
            parse_terminal_arguments(
                &json!({
                    "command": "job",
                    "notify_on_complete": false,
                    "watch_patterns": []
                })
                .to_string(),
                false
            )
            .is_ok()
        );
        assert!(
            parse_terminal_arguments(
                &json!({"command": "job", "background": true, "notify_on_complete": true})
                    .to_string(),
                true
            )
            .is_ok()
        );
        assert!(
            parse_terminal_arguments(
                &json!({"command": "job", "background": true, "watch_patterns": ["ready"]})
                    .to_string(),
                true
            )
            .is_ok()
        );
        assert!(
            parse_terminal_arguments(
                &json!({
                    "command": "job",
                    "background": true,
                    "watch_patterns": ["\u{754c}".repeat(MAX_WATCH_PATTERN_CHARS)]
                })
                .to_string(),
                true
            )
            .is_ok()
        );

        for raw in [
            json!({"command": "job", "notify_on_complete": true}),
            json!({"command": "job", "watch_patterns": ["ready"]}),
            json!({
                "command": "job",
                "background": true,
                "notify_on_complete": true,
                "watch_patterns": ["ready"]
            }),
            json!({"command": "job", "background": true, "watch_patterns": [""]}),
            json!({
                "command": "job",
                "background": true,
                "watch_patterns": ["x".repeat(MAX_WATCH_PATTERN_CHARS + 1)]
            }),
            json!({
                "command": "job",
                "background": true,
                "watch_patterns": (0..=MAX_WATCH_PATTERNS).map(|index| format!("p{index}")).collect::<Vec<_>>()
            }),
        ] {
            assert_eq!(
                parse_terminal_arguments(&raw.to_string(), true),
                Err(TerminalContractError::InvalidArguments)
            );
        }
    }

    #[test]
    fn process_actions_encode_valid_combinations_and_dynamic_risk() {
        let cases = [
            (
                json!({"action": "list"}),
                ProcessAction::List,
                ToolRisk::ReadOnly,
            ),
            (
                json!({"action": "poll", "session_id": "proc_1"}),
                ProcessAction::Poll,
                ToolRisk::ReadOnly,
            ),
            (
                json!({"action": "log", "session_id": "proc_1"}),
                ProcessAction::Log,
                ToolRisk::ReadOnly,
            ),
            (
                json!({"action": "wait", "session_id": "proc_1", "timeout": 30}),
                ProcessAction::Wait,
                ToolRisk::ReadOnly,
            ),
            (
                json!({"action": "kill", "session_id": "proc_1"}),
                ProcessAction::Kill,
                ToolRisk::ApprovalRequired,
            ),
            (
                json!({"action": "write", "session_id": "proc_1", "data": "raw"}),
                ProcessAction::Write,
                ToolRisk::ApprovalRequired,
            ),
            (
                json!({"action": "submit", "session_id": "proc_1", "data": ""}),
                ProcessAction::Submit,
                ToolRisk::ApprovalRequired,
            ),
            (
                json!({"action": "close", "session_id": "proc_1"}),
                ProcessAction::Close,
                ToolRisk::ApprovalRequired,
            ),
        ];
        for (raw, action, risk) in cases {
            let parsed = parse_process_arguments(&raw.to_string()).unwrap();
            assert_eq!(parsed.action(), action);
            assert_eq!(parsed.risk(), risk);
            assert_eq!(
                process_input_summary(&parsed),
                format!("Process {}", action.as_str())
            );
            let summary = process_approval_summary(&parsed, Some("`data`"));
            assert!(!summary.is_empty());
            if action != ProcessAction::List {
                assert!(summary.contains("proc_1"));
            }
            if matches!(action, ProcessAction::Write | ProcessAction::Submit) {
                assert!(summary.contains("`data`"));
            }
        }

        assert_eq!(
            parse_process_arguments(r#"{"action":"log","session_id":"proc_1"}"#),
            Ok(ProcessArguments::Log {
                session_id: "proc_1".to_owned(),
                offset: 0,
                limit: DEFAULT_LOG_LIMIT,
            })
        );
        assert_eq!(
            parse_process_arguments(
                r#"{"action":"log","session_id":"proc_1","offset":5,"limit":10}"#
            ),
            Ok(ProcessArguments::Log {
                session_id: "proc_1".to_owned(),
                offset: 5,
                limit: 10,
            })
        );
    }

    #[test]
    fn process_rejects_unknown_fields_and_every_invalid_action_combination() {
        for raw in [
            r#"{}"#,
            r#"{"action":"unknown"}"#,
            r#"{"action":null}"#,
            r#"{"action":"list","session_id":"proc_1"}"#,
            r#"{"action":"list","data":"x"}"#,
            r#"{"action":"poll"}"#,
            r#"{"action":"poll","session_id":"proc_1","timeout":1}"#,
            r#"{"action":"log","session_id":"proc_1","timeout":1}"#,
            r#"{"action":"log","session_id":"proc_1","limit":0}"#,
            r#"{"action":"wait","session_id":"proc_1","offset":0}"#,
            r#"{"action":"wait","session_id":"proc_1","timeout":0}"#,
            r#"{"action":"kill","session_id":"proc_1","data":"x"}"#,
            r#"{"action":"write","session_id":"proc_1"}"#,
            r#"{"action":"write","session_id":"proc_1","limit":1,"data":"x"}"#,
            r#"{"action":"submit","session_id":"proc_1"}"#,
            r#"{"action":"close","session_id":"proc_1","data":"x"}"#,
            r#"{"action":"close","session_id":""}"#,
            r#"{"action":"close","session_id":"bad id"}"#,
            r#"{"action":"list","task_id":"internal"}"#,
            r#"{"action":"list","force":true}"#,
            "[]",
        ] {
            assert!(
                parse_process_arguments(raw).is_err(),
                "invalid process input accepted: {raw:?}"
            );
        }

        let data_at_limit = "x".repeat(MAX_STDIN_BYTES);
        assert!(
            parse_process_arguments(
                &json!({"action": "write", "session_id": "proc_1", "data": data_at_limit})
                    .to_string()
            )
            .is_ok()
        );
        let oversized_data = "x".repeat(MAX_STDIN_BYTES + 1);
        assert_eq!(
            parse_process_arguments(
                &json!({"action": "write", "session_id": "proc_1", "data": oversized_data})
                    .to_string()
            ),
            Err(TerminalContractError::InvalidArguments)
        );
        let glyph = "\u{754c}";
        let utf8_data_at_limit = glyph.repeat(MAX_STDIN_BYTES / glyph.len());
        assert!(
            parse_process_arguments(
                &json!({
                    "action": "write",
                    "session_id": "proc_1",
                    "data": utf8_data_at_limit
                })
                .to_string()
            )
            .is_ok()
        );
        let oversized_utf8_data = format!("{utf8_data_at_limit}{glyph}");
        assert_eq!(
            parse_process_arguments(
                &json!({
                    "action": "write",
                    "session_id": "proc_1",
                    "data": oversized_utf8_data
                })
                .to_string()
            ),
            Err(TerminalContractError::InvalidArguments)
        );
        assert_eq!(
            parse_process_arguments(&"x".repeat(MAX_ARGUMENT_BYTES + 1)),
            Err(TerminalContractError::InputTooLarge)
        );
    }

    #[test]
    fn result_contracts_serialize_with_hermes_field_names() {
        assert_eq!(
            serde_json::to_value(TerminalForegroundResult {
                output: "done".to_owned(),
                exit_code: 0,
                error: None,
            })
            .unwrap(),
            json!({"output": "done", "exit_code": 0, "error": null})
        );
        assert_eq!(
            serde_json::to_value(TerminalBackgroundResult {
                output: "Background process started".to_owned(),
                session_id: "proc_1".to_owned(),
                pid: Some(42),
                status: BackgroundProcessStatus::Running,
                exit_code: 0,
                error: None,
            })
            .unwrap(),
            json!({
                "output": "Background process started",
                "session_id": "proc_1",
                "pid": 42,
                "status": "running",
                "exit_code": 0,
                "error": null
            })
        );
        assert_eq!(
            serde_json::to_value(ProcessListResult {
                processes: vec![ProcessSummary {
                    session_id: "proc_1".to_owned(),
                    command_preview: "redacted command".to_owned(),
                    status: BackgroundProcessStatus::Running,
                    pid: Some(42),
                }],
            })
            .unwrap()["processes"][0]["command_preview"],
            "redacted command"
        );
        assert_eq!(
            serde_json::to_value(ProcessStatusResult {
                session_id: "proc_1".to_owned(),
                status: "exited".to_owned(),
                output: Some("done".to_owned()),
                exit_code: Some(0),
                error: None,
            })
            .unwrap()["status"],
            "exited"
        );
        assert_eq!(
            serde_json::to_value(ProcessLogResult {
                session_id: "proc_1".to_owned(),
                offset: 0,
                lines: vec!["line".to_owned()],
                next_offset: Some(1),
                total_lines: 1,
            })
            .unwrap()["next_offset"],
            1
        );
    }
}
