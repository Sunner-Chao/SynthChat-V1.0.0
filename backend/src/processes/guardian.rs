use std::{
    collections::BTreeSet,
    env,
    ffi::OsStr,
    fmt,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;
use zeroize::Zeroize;

use super::platform;

pub const PROCESS_GUARDIAN_MODE_ARG: &str = "--synthchat-process-guardian";
pub const PROCESS_GUARDIAN_PROTOCOL_EXIT: i32 = 64;
pub const PROCESS_GUARDIAN_RUNTIME_EXIT: i32 = 70;

const FRAME_MAGIC: &[u8; 8] = b"SCGRD001";
const FRAME_HEADER_BYTES: usize = FRAME_MAGIC.len() + size_of::<u32>();
const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_EXECUTABLE_BYTES: usize = 4 * 1024;
const MAX_CWD_BYTES: usize = 4 * 1024;
const MAX_SCRIPT_BYTES: usize = 32 * 1024;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_ARGUMENTS_BYTES: usize = 128 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_ENVIRONMENT_NAME_BYTES: usize = 128;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;
const MAX_ENVIRONMENT_BYTES: usize = 128 * 1024;
const FRAME_VERSION: u8 = 2;
const CONTROL_MAGIC: &[u8; 8] = b"SCCTL001";
const CONTROL_HEADER_BYTES: usize = CONTROL_MAGIC.len() + 1 + size_of::<u32>();
const CONTROL_WRITE: u8 = 1;
const CONTROL_CLOSE: u8 = 2;
const MAX_CONTROL_DATA_BYTES: usize = 32 * 1024;
const SCRIPT_ENVIRONMENT_NAME: &str = "SYNTHCHAT_GUARDIAN_SCRIPT";
pub(crate) const CODE_RPC_PORT_ENVIRONMENT_NAME: &str = "SYNTHCHAT_CODE_RPC_PORT";
pub(crate) const CODE_RPC_TOKEN_ENVIRONMENT_NAME: &str = "SYNTHCHAT_CODE_RPC_TOKEN";
const POSIX_WRAPPER: &str =
    "script=$SYNTHCHAT_GUARDIAN_SCRIPT; unset SYNTHCHAT_GUARDIAN_SCRIPT; eval \"$script\"";
const MIN_CODE_RPC_TOKEN_BYTES: usize = 32;
const MAX_CODE_RPC_TOKEN_BYTES: usize = 256;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProcessGuardianError {
    #[error("process guardian launch configuration is invalid")]
    InvalidConfiguration,
    #[error("process guardian launch frame exceeds its fixed limit")]
    FrameTooLarge,
    #[error("process guardian launch frame is invalid")]
    InvalidFrame,
    #[error("process guardian I/O failed")]
    Io,
    #[error("process guardian could not start the configured process")]
    SpawnFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessGuardianLaunch {
    version: u8,
    cwd: String,
    environment: Vec<ProcessGuardianEnvironment>,
    target: ProcessGuardianTarget,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProcessGuardianEnvironment {
    name: String,
    value: String,
}

impl fmt::Debug for ProcessGuardianEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessGuardianEnvironment")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProcessGuardianTarget {
    Shell {
        executable: String,
        script: String,
    },
    Direct {
        executable: String,
        arguments: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code_rpc: Option<CodeRpcBootstrap>,
    },
}

impl fmt::Debug for ProcessGuardianTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shell { executable, .. } => formatter
                .debug_struct("Shell")
                .field("executable", executable)
                .field("script", &"[redacted]")
                .finish(),
            Self::Direct {
                executable,
                arguments,
                code_rpc,
            } => formatter
                .debug_struct("Direct")
                .field("executable", executable)
                .field(
                    "arguments",
                    &format_args!("[redacted; {} argument(s)]", arguments.len()),
                )
                .field("code_rpc", code_rpc)
                .finish(),
        }
    }
}

/// Per-execution credentials which only the guardian may project into the
/// direct child's environment. Generic launch environment entries continue to
/// reject token-bearing and `SYNTHCHAT_*` names.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeRpcBootstrap {
    port: u16,
    token: String,
}

impl CodeRpcBootstrap {
    pub(crate) fn new(port: u16, token: impl Into<String>) -> Result<Self, ProcessGuardianError> {
        let bootstrap = Self {
            port,
            token: token.into(),
        };
        bootstrap.validate()?;
        Ok(bootstrap)
    }

    fn validate(&self) -> Result<(), ProcessGuardianError> {
        if self.port == 0
            || !(MIN_CODE_RPC_TOKEN_BYTES..=MAX_CODE_RPC_TOKEN_BYTES).contains(&self.token.len())
            || !self
                .token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ProcessGuardianError::InvalidConfiguration);
        }
        Ok(())
    }

    fn apply(&self, command: &mut StdCommand) {
        command.env(CODE_RPC_PORT_ENVIRONMENT_NAME, self.port.to_string());
        command.env(CODE_RPC_TOKEN_ENVIRONMENT_NAME, &self.token);
    }
}

impl fmt::Debug for CodeRpcBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeRpcBootstrap")
            .field("port", &self.port)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl Drop for CodeRpcBootstrap {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl ProcessGuardianLaunch {
    pub fn new(
        executable: &Path,
        cwd: &Path,
        script: impl Into<String>,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, ProcessGuardianError> {
        let launch = Self {
            version: FRAME_VERSION,
            cwd: cwd
                .to_str()
                .ok_or(ProcessGuardianError::InvalidConfiguration)?
                .to_owned(),
            environment: environment
                .into_iter()
                .map(|(name, value)| ProcessGuardianEnvironment { name, value })
                .collect(),
            target: ProcessGuardianTarget::Shell {
                executable: executable
                    .to_str()
                    .ok_or(ProcessGuardianError::InvalidConfiguration)?
                    .to_owned(),
                script: script.into(),
            },
        };
        launch.validate()?;
        Ok(launch)
    }

    pub(crate) fn new_direct(
        executable: &Path,
        cwd: &Path,
        arguments: impl IntoIterator<Item = String>,
        environment: impl IntoIterator<Item = (String, String)>,
        code_rpc: Option<CodeRpcBootstrap>,
    ) -> Result<Self, ProcessGuardianError> {
        let launch = Self {
            version: FRAME_VERSION,
            cwd: cwd
                .to_str()
                .ok_or(ProcessGuardianError::InvalidConfiguration)?
                .to_owned(),
            environment: environment
                .into_iter()
                .map(|(name, value)| ProcessGuardianEnvironment { name, value })
                .collect(),
            target: ProcessGuardianTarget::Direct {
                executable: executable
                    .to_str()
                    .ok_or(ProcessGuardianError::InvalidConfiguration)?
                    .to_owned(),
                arguments: arguments.into_iter().collect(),
                code_rpc,
            },
        };
        launch.validate()?;
        Ok(launch)
    }

    fn validate(&self) -> Result<(), ProcessGuardianError> {
        if self.version != FRAME_VERSION
            || !valid_absolute_path(&self.cwd, MAX_CWD_BYTES)
            || self.environment.len() > MAX_ENVIRONMENT_ENTRIES
        {
            return Err(ProcessGuardianError::InvalidConfiguration);
        }

        match &self.target {
            ProcessGuardianTarget::Shell { executable, script } => {
                if !valid_absolute_path(executable, MAX_EXECUTABLE_BYTES)
                    || script.is_empty()
                    || script.len() > MAX_SCRIPT_BYTES
                    || script.contains('\0')
                {
                    return Err(ProcessGuardianError::InvalidConfiguration);
                }
            }
            ProcessGuardianTarget::Direct {
                executable,
                arguments,
                code_rpc,
            } => {
                if !valid_absolute_path(executable, MAX_EXECUTABLE_BYTES)
                    || arguments.len() > MAX_ARGUMENTS
                {
                    return Err(ProcessGuardianError::InvalidConfiguration);
                }
                let mut total_bytes = 0_usize;
                for argument in arguments {
                    if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
                        return Err(ProcessGuardianError::InvalidConfiguration);
                    }
                    total_bytes = total_bytes
                        .checked_add(argument.len())
                        .ok_or(ProcessGuardianError::InvalidConfiguration)?;
                }
                if total_bytes > MAX_ARGUMENTS_BYTES {
                    return Err(ProcessGuardianError::InvalidConfiguration);
                }
                if let Some(code_rpc) = code_rpc {
                    code_rpc.validate()?;
                }
            }
        }

        let mut names = BTreeSet::new();
        let mut total_bytes = 0_usize;
        for entry in &self.environment {
            let normalized = entry.name.to_ascii_uppercase();
            if entry.name.is_empty()
                || entry.name.len() > MAX_ENVIRONMENT_NAME_BYTES
                || entry.name.contains(['\0', '='])
                || entry.value.len() > MAX_ENVIRONMENT_VALUE_BYTES
                || entry.value.contains('\0')
                || normalized == SCRIPT_ENVIRONMENT_NAME
                || sensitive_environment_name(&entry.name)
                || !names.insert(normalized)
            {
                return Err(ProcessGuardianError::InvalidConfiguration);
            }
            total_bytes = total_bytes
                .checked_add(entry.name.len())
                .and_then(|total| total.checked_add(entry.value.len()))
                .ok_or(ProcessGuardianError::InvalidConfiguration)?;
        }
        if total_bytes > MAX_ENVIRONMENT_BYTES {
            return Err(ProcessGuardianError::InvalidConfiguration);
        }
        Ok(())
    }

    fn executable(&self) -> PathBuf {
        match &self.target {
            ProcessGuardianTarget::Shell { executable, .. }
            | ProcessGuardianTarget::Direct { executable, .. } => PathBuf::from(executable),
        }
    }

    fn cwd(&self) -> PathBuf {
        PathBuf::from(&self.cwd)
    }
}

pub fn encode_process_guardian_launch(
    launch: &ProcessGuardianLaunch,
) -> Result<Vec<u8>, ProcessGuardianError> {
    launch.validate()?;
    let mut payload = serde_json::to_vec(launch).map_err(|_| ProcessGuardianError::InvalidFrame)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProcessGuardianError::FrameTooLarge);
    }
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| ProcessGuardianError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(FRAME_MAGIC);
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&payload);
    payload.zeroize();
    Ok(frame)
}

pub fn encode_process_guardian_stdin(data: &[u8]) -> Result<Vec<u8>, ProcessGuardianError> {
    encode_control_frame(CONTROL_WRITE, data)
}

pub fn encode_process_guardian_stdin_close() -> Vec<u8> {
    encode_control_frame(CONTROL_CLOSE, &[])
        .expect("the fixed guardian close control frame is valid")
}

pub fn process_guardian_command(
    backend_executable: &Path,
    foreground: bool,
) -> io::Result<Command> {
    if !backend_executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process guardian executable must be absolute",
        ));
    }
    let mut command = Command::new(backend_executable);
    command
        .arg(PROCESS_GUARDIAN_MODE_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    platform::configure_spawn(&mut command, foreground)?;
    Ok(command)
}

pub fn process_guardian_mode_requested() -> bool {
    env::args_os().nth(1).as_deref() == Some(OsStr::new(PROCESS_GUARDIAN_MODE_ARG))
}

pub fn run_process_guardian_stdio() -> i32 {
    if !valid_guardian_invocation() {
        return PROCESS_GUARDIAN_PROTOCOL_EXIT;
    }
    match run_process_guardian(std::io::stdin()) {
        Ok(exit_code) => exit_code,
        Err(ProcessGuardianError::InvalidConfiguration)
        | Err(ProcessGuardianError::FrameTooLarge)
        | Err(ProcessGuardianError::InvalidFrame) => PROCESS_GUARDIAN_PROTOCOL_EXIT,
        Err(ProcessGuardianError::Io) | Err(ProcessGuardianError::SpawnFailed) => {
            PROCESS_GUARDIAN_RUNTIME_EXIT
        }
    }
}

fn run_process_guardian<R>(mut input: R) -> Result<i32, ProcessGuardianError>
where
    R: Read + Send + 'static,
{
    let launch = read_launch(&mut input)?;
    let executable = launch.executable();
    let cwd = launch.cwd();
    if !std::fs::metadata(&executable).is_ok_and(|metadata| metadata.is_file())
        || !std::fs::metadata(&cwd).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(ProcessGuardianError::InvalidConfiguration);
    }

    let mut command = StdCommand::new(&executable);
    match &launch.target {
        ProcessGuardianTarget::Shell { script, .. } => {
            let _ = script;
            command
                .args(shell_arguments(&executable))
                .arg(POSIX_WRAPPER);
        }
        ProcessGuardianTarget::Direct {
            arguments,
            code_rpc,
            ..
        } => {
            command.args(arguments);
            let _ = code_rpc;
        }
    }
    command
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for entry in &launch.environment {
        command.env(&entry.name, &entry.value);
    }
    // `env_clear` above also removes values applied while configuring the
    // target, so project the typed-only values after the generic environment.
    match &launch.target {
        ProcessGuardianTarget::Shell { script, .. } => {
            command.env(SCRIPT_ENVIRONMENT_NAME, script);
        }
        ProcessGuardianTarget::Direct {
            code_rpc: Some(code_rpc),
            ..
        } => code_rpc.apply(&mut command),
        ProcessGuardianTarget::Direct { code_rpc: None, .. } => {}
    }

    let mut child = command
        .spawn()
        .map_err(|_| ProcessGuardianError::SpawnFailed)?;
    #[cfg(target_os = "windows")]
    let lifetime = match platform::process_lifetime(child.id()) {
        Ok(lifetime) => lifetime,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProcessGuardianError::SpawnFailed);
        }
    };
    #[cfg(not(target_os = "windows"))]
    let lifetime = ();
    let Some(child_stdin) = child.stdin.take() else {
        terminate_after_parent_disconnect(&mut child, &lifetime);
        return Err(ProcessGuardianError::SpawnFailed);
    };
    let (event_sender, event_receiver) = mpsc::channel();
    let forwarding = thread::Builder::new()
        .name("synthchat-process-guardian-stdin".to_owned())
        .spawn(move || {
            forward_control_stream(input, child_stdin);
            let _ = event_sender.send(());
        });
    if forwarding.is_err() {
        terminate_after_parent_disconnect(&mut child, &lifetime);
        return Err(ProcessGuardianError::Io);
    }
    loop {
        if let Some(status) = child.try_wait().map_err(|_| ProcessGuardianError::Io)? {
            return Ok(exit_status_code(status));
        }
        match event_receiver.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                terminate_after_parent_disconnect(&mut child, &lifetime);
                return Err(ProcessGuardianError::Io);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn encode_control_frame(kind: u8, data: &[u8]) -> Result<Vec<u8>, ProcessGuardianError> {
    if !matches!(kind, CONTROL_WRITE | CONTROL_CLOSE)
        || data.len() > MAX_CONTROL_DATA_BYTES
        || kind == CONTROL_CLOSE && !data.is_empty()
    {
        return Err(ProcessGuardianError::InvalidConfiguration);
    }
    let length = u32::try_from(data.len()).map_err(|_| ProcessGuardianError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(CONTROL_HEADER_BYTES + data.len());
    frame.extend_from_slice(CONTROL_MAGIC);
    frame.push(kind);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(data);
    Ok(frame)
}

fn forward_control_stream<R: Read>(mut input: R, child_stdin: std::process::ChildStdin) {
    let mut child_stdin = Some(child_stdin);
    loop {
        let mut magic = [0_u8; CONTROL_MAGIC.len()];
        if input.read_exact(&mut magic).is_err() || &magic != CONTROL_MAGIC {
            return;
        }
        let mut kind = [0_u8; 1];
        let mut encoded_len = [0_u8; size_of::<u32>()];
        if input.read_exact(&mut kind).is_err() || input.read_exact(&mut encoded_len).is_err() {
            return;
        }
        let Ok(length) = usize::try_from(u32::from_be_bytes(encoded_len)) else {
            return;
        };
        if length > MAX_CONTROL_DATA_BYTES
            || !matches!(kind[0], CONTROL_WRITE | CONTROL_CLOSE)
            || kind[0] == CONTROL_CLOSE && length != 0
            || child_stdin.is_none() && kind[0] == CONTROL_WRITE
        {
            return;
        }
        let mut data = vec![0_u8; length];
        if input.read_exact(&mut data).is_err() {
            return;
        }
        match kind[0] {
            CONTROL_WRITE => {
                let Some(stdin) = child_stdin.as_mut() else {
                    return;
                };
                if stdin.write_all(&data).is_err() || stdin.flush().is_err() {
                    return;
                }
            }
            CONTROL_CLOSE => {
                drop(child_stdin.take());
            }
            _ => return,
        }
    }
}

#[cfg(target_os = "windows")]
fn terminate_after_parent_disconnect(
    child: &mut std::process::Child,
    lifetime: &platform::ProcessLifetime,
) {
    // The Job handle is retained from target startup, so this termination is
    // bound to the exact process tree without spawning an external terminator or resolving a
    // PID after the parent control pipe has disconnected.
    let _ = platform::terminate_lifetime_now(lifetime);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_after_parent_disconnect(_child: &mut std::process::Child, _lifetime: &()) {
    let group = unsafe { libc::getpgrp() };
    if group > 0 {
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    std::process::abort();
}

fn read_launch<R: Read>(input: &mut R) -> Result<ProcessGuardianLaunch, ProcessGuardianError> {
    let mut magic = [0_u8; FRAME_MAGIC.len()];
    input
        .read_exact(&mut magic)
        .map_err(|_| ProcessGuardianError::InvalidFrame)?;
    if &magic != FRAME_MAGIC {
        return Err(ProcessGuardianError::InvalidFrame);
    }
    let mut encoded_len = [0_u8; size_of::<u32>()];
    input
        .read_exact(&mut encoded_len)
        .map_err(|_| ProcessGuardianError::InvalidFrame)?;
    let payload_len = u32::from_be_bytes(encoded_len);
    let payload_len =
        usize::try_from(payload_len).map_err(|_| ProcessGuardianError::InvalidFrame)?;
    if payload_len == 0 || payload_len > MAX_FRAME_BYTES {
        return Err(ProcessGuardianError::FrameTooLarge);
    }
    let mut payload = vec![0_u8; payload_len];
    input
        .read_exact(&mut payload)
        .map_err(|_| ProcessGuardianError::InvalidFrame)?;
    let launch: ProcessGuardianLaunch =
        serde_json::from_slice(&payload).map_err(|_| ProcessGuardianError::InvalidFrame)?;
    launch.validate()?;
    Ok(launch)
}

fn valid_guardian_invocation() -> bool {
    let mut arguments = env::args_os().skip(1);
    arguments.next().as_deref() == Some(OsStr::new(PROCESS_GUARDIAN_MODE_ARG))
        && arguments.next().is_none()
}

fn valid_absolute_path(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
}

fn shell_arguments(executable: &Path) -> &'static [&'static str] {
    let name = executable
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    if name.eq_ignore_ascii_case("zsh") {
        &["-f", "-c"]
    } else {
        &["--noprofile", "--norc", "-lc"]
    }
}

fn sensitive_environment_name(name: &str) -> bool {
    let normalized = name.to_ascii_uppercase();
    normalized.starts_with("SYNTHCHAT_")
        || normalized.contains("TOKEN")
        || normalized.contains("SECRET")
        || normalized.contains("PASSWORD")
        || normalized.contains("PASSWD")
        || normalized.contains("API_KEY")
        || normalized.contains("APIKEY")
        || normalized.contains("CREDENTIAL")
        || normalized.contains("PRIVATE_KEY")
        || normalized.contains("ACCESS_KEY")
        || normalized.contains("AUTH_SOCK")
        || normalized.contains("CONNECTION_STRING")
        || normalized == "DATABASE_URL"
        || normalized.ends_with("_DSN")
        || matches!(
            normalized.as_str(),
            "KUBECONFIG" | "DOCKER_HOST" | "DOCKER_CERT_PATH" | "SSH_AGENT_PID" | "GPG_AGENT_INFO"
        )
}

fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128_i32.saturating_add(signal);
        }
    }
    PROCESS_GUARDIAN_RUNTIME_EXIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_frame_round_trips_without_consuming_following_stdin() {
        let cwd = std::env::current_dir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let launch = ProcessGuardianLaunch::new(
            &executable,
            &cwd,
            "printf ok",
            [("PATH".to_owned(), "safe".to_owned())],
        )
        .unwrap();
        let mut frame = encode_process_guardian_launch(&launch).unwrap();
        frame.extend_from_slice(b"remaining stdin");
        let mut input = frame.as_slice();
        assert_eq!(read_launch(&mut input).unwrap(), launch);
        let mut remaining = Vec::new();
        input.read_to_end(&mut remaining).unwrap();
        assert_eq!(remaining, b"remaining stdin");
    }

    #[test]
    fn launch_rejects_sensitive_duplicate_and_oversized_environment() {
        let cwd = std::env::current_dir().unwrap();
        let executable = std::env::current_exe().unwrap();
        for environment in [
            vec![("OPENAI_API_KEY".to_owned(), "secret".to_owned())],
            vec![
                ("Path".to_owned(), "one".to_owned()),
                ("PATH".to_owned(), "two".to_owned()),
            ],
            vec![(
                "SAFE".to_owned(),
                "x".repeat(MAX_ENVIRONMENT_VALUE_BYTES + 1),
            )],
        ] {
            assert_eq!(
                ProcessGuardianLaunch::new(&executable, &cwd, "true", environment),
                Err(ProcessGuardianError::InvalidConfiguration)
            );
        }
    }

    #[test]
    fn direct_launch_is_tagged_and_rpc_credentials_are_debug_redacted() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let cwd = std::env::current_dir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let bootstrap = CodeRpcBootstrap::new(8642, TOKEN).unwrap();
        let launch = ProcessGuardianLaunch::new_direct(
            &executable,
            &cwd,
            ["private-script-path.py".to_owned()],
            [("PYTHONUTF8".to_owned(), "1".to_owned())],
            Some(bootstrap),
        )
        .unwrap();

        let debug = format!("{launch:?}");
        assert!(!debug.contains(TOKEN));
        assert!(!debug.contains("private-script-path.py"));
        assert!(!debug.contains("PYTHONUTF8\": \"1"));
        assert!(debug.contains("Direct"));
        assert!(debug.contains("[redacted"));

        let frame = encode_process_guardian_launch(&launch).unwrap();
        let mut input = frame.as_slice();
        let decoded = read_launch(&mut input).unwrap();
        assert_eq!(decoded, launch);
        assert!(matches!(
            decoded.target,
            ProcessGuardianTarget::Direct { .. }
        ));
    }

    #[test]
    fn generic_environment_cannot_smuggle_typed_rpc_credentials() {
        let cwd = std::env::current_dir().unwrap();
        let executable = std::env::current_exe().unwrap();
        for name in [
            CODE_RPC_PORT_ENVIRONMENT_NAME,
            CODE_RPC_TOKEN_ENVIRONMENT_NAME,
        ] {
            assert_eq!(
                ProcessGuardianLaunch::new_direct(
                    &executable,
                    &cwd,
                    Vec::<String>::new(),
                    [(name.to_owned(), "not-allowed".to_owned())],
                    None,
                ),
                Err(ProcessGuardianError::InvalidConfiguration)
            );
        }
    }

    #[test]
    fn code_rpc_bootstrap_validates_loopback_port_and_opaque_token_shape() {
        let valid = "a".repeat(MIN_CODE_RPC_TOKEN_BYTES);
        assert!(CodeRpcBootstrap::new(1, valid).is_ok());
        assert_eq!(
            CodeRpcBootstrap::new(0, "a".repeat(MIN_CODE_RPC_TOKEN_BYTES)),
            Err(ProcessGuardianError::InvalidConfiguration)
        );
        assert_eq!(
            CodeRpcBootstrap::new(8642, "too-short"),
            Err(ProcessGuardianError::InvalidConfiguration)
        );
        assert_eq!(
            CodeRpcBootstrap::new(8642, format!("{}!", "a".repeat(31))),
            Err(ProcessGuardianError::InvalidConfiguration)
        );
    }

    #[test]
    fn truncated_frame_is_rejected_before_launch_is_available() {
        let cwd = std::env::current_dir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let launch = ProcessGuardianLaunch::new(&executable, &cwd, "printf bad", []).unwrap();
        let frame = encode_process_guardian_launch(&launch).unwrap();
        let mut input = &frame[..frame.len() - 1];
        assert_eq!(
            read_launch(&mut input),
            Err(ProcessGuardianError::InvalidFrame)
        );
    }
}
