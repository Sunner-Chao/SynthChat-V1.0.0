use std::{
    env,
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

use crate::runtime_config::DesktopRuntimeConfig;

const DEFAULT_BACKEND_ADDR: &str = "127.0.0.1:0";
const STARTUP_HANDSHAKE_PREFIX: &str = "SYNTHCHAT_BACKEND_READY ";
const MAX_STARTUP_HANDSHAKE_BYTES: usize = 128;
const MAX_PROBE_RESPONSE_BYTES: usize = 1_024;
const DEVELOPMENT_ORIGIN: &str = "http://127.0.0.1:1421";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendConnection {
    base_url: String,
    token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum BackendLifecycleState {
    Starting,
    Ready,
    Stopping,
    Backoff,
    Failed,
    ShuttingDown,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    available: bool,
    managed: bool,
    base_url: Option<String>,
    state: BackendLifecycleState,
    generation: Option<u64>,
    error: Option<String>,
}

#[derive(Clone)]
struct BackendLaunchConfig {
    binary: PathBuf,
    configured_address: SocketAddr,
    development_origin: Option<String>,
}

impl BackendLaunchConfig {
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            binary: backend_binary()?,
            configured_address: configured_backend_address()?,
            development_origin: configured_development_origin()?,
        })
    }
}

trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

trait TokenSource: Send + Sync {
    fn generate(&self) -> Result<String, String>;
}

struct OsTokenSource;

impl TokenSource for OsTokenSource {
    fn generate(&self) -> Result<String, String> {
        generate_token()
    }
}

trait ManagedBackend: Send {
    fn try_wait(&mut self) -> Result<Option<String>, String>;
    fn stop(&mut self, runtime: &DesktopRuntimeConfig) -> Result<(), String>;
}

struct RealManagedBackend {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl ManagedBackend for RealManagedBackend {
    fn try_wait(&mut self) -> Result<Option<String>, String> {
        self.child
            .try_wait()
            .map(|status| status.map(|status| status.to_string()))
            .map_err(|_| "failed to inspect the managed backend process".to_owned())
    }

    fn stop(&mut self, runtime: &DesktopRuntimeConfig) -> Result<(), String> {
        stop_child(&mut self.child, self.stdin.take(), runtime)
    }
}

struct LaunchedBackend {
    child: Box<dyn ManagedBackend>,
    address: SocketAddr,
}

struct LaunchFailure {
    message: String,
    retry_safe: bool,
}

impl LaunchFailure {
    fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_safe: true,
        }
    }

    fn unsafe_to_retry(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_safe: false,
        }
    }
}

trait BackendLauncher: Send + Sync {
    fn launch_ready(
        &self,
        config: &BackendLaunchConfig,
        runtime: &DesktopRuntimeConfig,
        token: &str,
    ) -> Result<LaunchedBackend, LaunchFailure>;

    fn probe_authenticated(
        &self,
        address: SocketAddr,
        token: &str,
        runtime: &DesktopRuntimeConfig,
    ) -> bool;
}

struct RealBackendLauncher;

impl BackendLauncher for RealBackendLauncher {
    fn launch_ready(
        &self,
        config: &BackendLaunchConfig,
        runtime: &DesktopRuntimeConfig,
        token: &str,
    ) -> Result<LaunchedBackend, LaunchFailure> {
        launch_real_backend(config, runtime, token)
    }

    fn probe_authenticated(
        &self,
        address: SocketAddr,
        token: &str,
        runtime: &DesktopRuntimeConfig,
    ) -> bool {
        probe_authenticated(address, token, runtime.probe_timeout)
    }
}

#[derive(Clone)]
struct ReadyConnection {
    base_url: String,
    token: Arc<Zeroizing<String>>,
}

#[derive(Clone)]
struct BackendSnapshot {
    state: BackendLifecycleState,
    generation: Option<u64>,
    connection: Option<ReadyConnection>,
    error: Option<String>,
}

impl BackendSnapshot {
    fn starting(generation: Option<u64>) -> Self {
        Self {
            state: BackendLifecycleState::Starting,
            generation,
            connection: None,
            error: None,
        }
    }

    fn ready(generation: u64, address: SocketAddr, token: Arc<Zeroizing<String>>) -> Self {
        Self {
            state: BackendLifecycleState::Ready,
            generation: Some(generation),
            connection: Some(ReadyConnection {
                base_url: backend_base_url(address),
                token,
            }),
            error: None,
        }
    }

    fn unavailable(
        state: BackendLifecycleState,
        generation: Option<u64>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            state,
            generation,
            connection: None,
            error: Some(error.into()),
        }
    }

    fn shutting_down(error: Option<String>) -> Self {
        Self {
            state: BackendLifecycleState::ShuttingDown,
            generation: None,
            connection: None,
            error,
        }
    }

    fn status(&self) -> BackendStatus {
        let available = self.state == BackendLifecycleState::Ready && self.connection.is_some();
        let managed = matches!(
            self.state,
            BackendLifecycleState::Starting
                | BackendLifecycleState::Ready
                | BackendLifecycleState::Stopping
        );
        BackendStatus {
            available,
            managed,
            base_url: self
                .connection
                .as_ref()
                .map(|connection| connection.base_url.clone()),
            state: self.state,
            generation: self.generation,
            error: self.error.clone(),
        }
    }
}

struct SnapshotStore {
    current: Mutex<BackendSnapshot>,
    changed: Condvar,
}

impl SnapshotStore {
    fn new(initial: BackendSnapshot) -> Self {
        Self {
            current: Mutex::new(initial),
            changed: Condvar::new(),
        }
    }

    fn publish(&self, snapshot: BackendSnapshot) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = snapshot;
        self.changed.notify_all();
    }

    fn load(&self) -> Result<BackendSnapshot, String> {
        self.current
            .lock()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| "backend state is unavailable".to_owned())
    }
}

enum SupervisorCommand {
    Shutdown,
    #[cfg(test)]
    Wake,
}

pub struct BackendManager {
    snapshots: Arc<SnapshotStore>,
    command: Option<mpsc::Sender<SupervisorCommand>>,
    worker: Option<JoinHandle<()>>,
}

impl BackendManager {
    pub fn start() -> Self {
        Self::spawn_worker(|snapshots, receiver| {
            let runtime = match DesktopRuntimeConfig::from_env() {
                Ok(runtime) => runtime,
                Err(error) => {
                    snapshots.publish(BackendSnapshot::unavailable(
                        BackendLifecycleState::Failed,
                        None,
                        format!("invalid desktop runtime configuration: {error}"),
                    ));
                    return;
                }
            };
            let launch_config = match BackendLaunchConfig::from_env() {
                Ok(config) => config,
                Err(error) => {
                    snapshots.publish(BackendSnapshot::unavailable(
                        BackendLifecycleState::Failed,
                        None,
                        error,
                    ));
                    return;
                }
            };
            Supervisor {
                snapshots,
                launch_config,
                runtime,
                launcher: Arc::new(RealBackendLauncher),
                clock: Arc::new(SystemClock),
                token_source: Arc::new(OsTokenSource),
            }
            .run(receiver);
        })
    }

    #[cfg(test)]
    fn start_with_dependencies(
        launch_config: BackendLaunchConfig,
        runtime: DesktopRuntimeConfig,
        launcher: Arc<dyn BackendLauncher>,
        clock: Arc<dyn Clock>,
        token_source: Arc<dyn TokenSource>,
    ) -> Self {
        Self::spawn_worker(move |snapshots, receiver| {
            Supervisor {
                snapshots,
                launch_config,
                runtime,
                launcher,
                clock,
                token_source,
            }
            .run(receiver);
        })
    }

    fn spawn_worker(
        run: impl FnOnce(Arc<SnapshotStore>, mpsc::Receiver<SupervisorCommand>) + Send + 'static,
    ) -> Self {
        let snapshots = Arc::new(SnapshotStore::new(BackendSnapshot::starting(None)));
        let (sender, receiver) = mpsc::channel();
        let worker_snapshots = Arc::clone(&snapshots);
        match thread::Builder::new()
            .name("synthchat-backend-supervisor".to_owned())
            .spawn(move || run(worker_snapshots, receiver))
        {
            Ok(worker) => Self {
                snapshots,
                command: Some(sender),
                worker: Some(worker),
            },
            Err(_) => {
                snapshots.publish(BackendSnapshot::unavailable(
                    BackendLifecycleState::Failed,
                    None,
                    "failed to start the backend supervisor worker",
                ));
                Self {
                    snapshots,
                    command: None,
                    worker: None,
                }
            }
        }
    }

    pub fn connection(&self) -> Result<BackendConnection, String> {
        let snapshot = self.snapshots.load()?;
        match snapshot.connection {
            Some(connection) if snapshot.state == BackendLifecycleState::Ready => {
                Ok(BackendConnection {
                    base_url: connection.base_url,
                    token: connection.token.as_str().to_owned(),
                })
            }
            _ => Err(snapshot.error.unwrap_or_else(|| match snapshot.state {
                BackendLifecycleState::Starting => {
                    "managed backend startup is in progress".to_owned()
                }
                BackendLifecycleState::Stopping => "managed backend is stopping".to_owned(),
                BackendLifecycleState::Backoff => {
                    "managed backend restart is waiting for backoff".to_owned()
                }
                BackendLifecycleState::Failed => "managed backend startup failed".to_owned(),
                BackendLifecycleState::ShuttingDown => {
                    "managed backend is shutting down".to_owned()
                }
                BackendLifecycleState::Ready => "managed backend is unavailable".to_owned(),
            })),
        }
    }

    pub fn status(&self) -> BackendStatus {
        self.snapshots
            .load()
            .map(|snapshot| snapshot.status())
            .unwrap_or_else(|error| BackendStatus {
                available: false,
                managed: false,
                base_url: None,
                state: BackendLifecycleState::Failed,
                generation: None,
                error: Some(error),
            })
    }

    #[cfg(test)]
    fn wake_worker(&self) {
        if let Some(command) = self.command.as_ref() {
            let _ = command.send(SupervisorCommand::Wake);
        }
    }
}

impl Drop for BackendManager {
    fn drop(&mut self) {
        if let Some(command) = self.command.take() {
            let _ = command.send(SupervisorCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Supervisor {
    snapshots: Arc<SnapshotStore>,
    launch_config: BackendLaunchConfig,
    runtime: DesktopRuntimeConfig,
    launcher: Arc<dyn BackendLauncher>,
    clock: Arc<dyn Clock>,
    token_source: Arc<dyn TokenSource>,
}

struct BackendGeneration {
    number: u64,
    child: Box<dyn ManagedBackend>,
    token: Arc<Zeroizing<String>>,
    address: SocketAddr,
    ready_at: Instant,
}

enum ActiveOutcome {
    Restart { error: String, stable: bool },
    Shutdown,
    Terminal,
}

impl Supervisor {
    fn run(self, receiver: mpsc::Receiver<SupervisorCommand>) {
        let mut next_generation = 0_u64;
        let mut consecutive_failures = 0_u32;

        loop {
            if shutdown_requested(&receiver) {
                self.snapshots.publish(BackendSnapshot::shutting_down(None));
                return;
            }

            next_generation = next_generation.wrapping_add(1).max(1);
            let generation = next_generation;
            self.snapshots
                .publish(BackendSnapshot::starting(Some(generation)));

            let token = match self.new_token() {
                Ok(token) => token,
                Err(error) => {
                    if !self.wait_after_failure(
                        &receiver,
                        generation,
                        error,
                        &mut consecutive_failures,
                    ) {
                        return;
                    }
                    continue;
                }
            };

            let launched =
                self.launcher
                    .launch_ready(&self.launch_config, &self.runtime, token.as_str());

            if shutdown_requested(&receiver) {
                if let Ok(mut launched) = launched {
                    self.snapshots.publish(BackendSnapshot::unavailable(
                        BackendLifecycleState::Stopping,
                        Some(generation),
                        "desktop shutdown requested",
                    ));
                    let stop_error = launched.child.stop(&self.runtime).err();
                    self.snapshots
                        .publish(BackendSnapshot::shutting_down(stop_error));
                } else {
                    self.snapshots.publish(BackendSnapshot::shutting_down(None));
                }
                return;
            }

            let launched = match launched {
                Ok(launched) => launched,
                Err(error) if error.retry_safe => {
                    if !self.wait_after_failure(
                        &receiver,
                        generation,
                        error.message,
                        &mut consecutive_failures,
                    ) {
                        return;
                    }
                    continue;
                }
                Err(error) => {
                    self.snapshots.publish(BackendSnapshot::unavailable(
                        BackendLifecycleState::Failed,
                        Some(generation),
                        error.message,
                    ));
                    return;
                }
            };

            let active = BackendGeneration {
                number: generation,
                child: launched.child,
                token,
                address: launched.address,
                ready_at: self.clock.now(),
            };

            match self.supervise_active(active, &receiver) {
                ActiveOutcome::Restart { error, stable } => {
                    if stable {
                        consecutive_failures = 0;
                    }
                    if !self.wait_after_failure(
                        &receiver,
                        generation,
                        error,
                        &mut consecutive_failures,
                    ) {
                        return;
                    }
                }
                ActiveOutcome::Shutdown | ActiveOutcome::Terminal => return,
            }
        }
    }

    fn new_token(&self) -> Result<Arc<Zeroizing<String>>, String> {
        let token = Zeroizing::new(self.token_source.generate()?);
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("failed to generate a 256-bit backend session token".to_owned());
        }
        Ok(Arc::new(token))
    }

    fn supervise_active(
        &self,
        mut active: BackendGeneration,
        receiver: &mpsc::Receiver<SupervisorCommand>,
    ) -> ActiveOutcome {
        let mut stability_confirmed = false;
        self.snapshots.publish(BackendSnapshot::ready(
            active.number,
            active.address,
            Arc::clone(&active.token),
        ));

        loop {
            match receiver.recv_timeout(self.runtime.monitor_interval) {
                Ok(SupervisorCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.snapshots.publish(BackendSnapshot::unavailable(
                        BackendLifecycleState::Stopping,
                        Some(active.number),
                        "desktop shutdown requested",
                    ));
                    let stop_error = active.child.stop(&self.runtime).err();
                    self.snapshots
                        .publish(BackendSnapshot::shutting_down(stop_error));
                    return ActiveOutcome::Shutdown;
                }
                #[cfg(test)]
                Ok(SupervisorCommand::Wake) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            let failure = match active.child.try_wait() {
                Ok(Some(status)) => Some(format!("managed backend exited with status {status}")),
                Err(error) => Some(error),
                Ok(None)
                    if !self.launcher.probe_authenticated(
                        active.address,
                        active.token.as_str(),
                        &self.runtime,
                    ) =>
                {
                    Some("managed backend authentication probe failed".to_owned())
                }
                Ok(None) => None,
            };
            let Some(error) = failure else {
                if !stability_confirmed
                    && self.clock.now().saturating_duration_since(active.ready_at)
                        >= self.runtime.stable_window
                {
                    stability_confirmed = true;
                }
                continue;
            };

            self.snapshots.publish(BackendSnapshot::unavailable(
                BackendLifecycleState::Stopping,
                Some(active.number),
                error.clone(),
            ));

            if let Err(stop_error) = active.child.stop(&self.runtime) {
                self.snapshots.publish(BackendSnapshot::unavailable(
                    BackendLifecycleState::Failed,
                    Some(active.number),
                    format!(
                        "{error}; backend termination could not be confirmed: {stop_error}; automatic restart disabled"
                    ),
                ));
                return ActiveOutcome::Terminal;
            }

            if shutdown_requested(receiver) {
                self.snapshots.publish(BackendSnapshot::shutting_down(None));
                return ActiveOutcome::Shutdown;
            }
            return ActiveOutcome::Restart {
                error,
                stable: stability_confirmed,
            };
        }
    }

    fn wait_after_failure(
        &self,
        receiver: &mpsc::Receiver<SupervisorCommand>,
        generation: u64,
        error: String,
        consecutive_failures: &mut u32,
    ) -> bool {
        let delay = restart_backoff(&self.runtime, *consecutive_failures);
        *consecutive_failures = consecutive_failures.saturating_add(1);
        let retry_at = self.clock.now() + delay;
        self.snapshots.publish(BackendSnapshot::unavailable(
            BackendLifecycleState::Backoff,
            Some(generation),
            format!(
                "{error}; managed backend retry scheduled in {} ms",
                delay.as_millis()
            ),
        ));

        loop {
            if self.clock.now() >= retry_at {
                return true;
            }
            let remaining = retry_at.saturating_duration_since(self.clock.now());
            match receiver.recv_timeout(remaining.min(self.runtime.monitor_interval)) {
                Ok(SupervisorCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.snapshots.publish(BackendSnapshot::shutting_down(None));
                    return false;
                }
                #[cfg(test)]
                Ok(SupervisorCommand::Wake) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(not(test))]
fn shutdown_requested(receiver: &mpsc::Receiver<SupervisorCommand>) -> bool {
    matches!(
        receiver.try_recv(),
        Ok(SupervisorCommand::Shutdown) | Err(mpsc::TryRecvError::Disconnected)
    )
}

#[cfg(test)]
fn shutdown_requested(receiver: &mpsc::Receiver<SupervisorCommand>) -> bool {
    loop {
        match receiver.try_recv() {
            Ok(SupervisorCommand::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return true,
            Ok(SupervisorCommand::Wake) => {}
            Err(mpsc::TryRecvError::Empty) => return false,
        }
    }
}

fn restart_backoff(runtime: &DesktopRuntimeConfig, consecutive_failures: u32) -> Duration {
    let mut delay = runtime.restart_backoff_initial;
    for _ in 0..consecutive_failures.min(31) {
        delay = delay.saturating_mul(2).min(runtime.restart_backoff_max);
    }
    delay.min(runtime.restart_backoff_max)
}

fn generate_token() -> Result<String, String> {
    generate_token_with(|bytes| getrandom::fill(bytes).map_err(|_| ()))
}

fn generate_token_with(fill: impl FnOnce(&mut [u8]) -> Result<(), ()>) -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    if fill(&mut bytes).is_err() {
        bytes.zeroize();
        return Err("operating-system randomness is unavailable".to_owned());
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(bytes.len() * 2);
    for &byte in &bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    bytes.zeroize();
    Ok(token)
}

fn configured_backend_address() -> Result<SocketAddr, String> {
    match env::var("SYNTHCHAT_BACKEND_ADDR") {
        Ok(value) => parse_configured_address(Some(&value)),
        Err(env::VarError::NotPresent) => parse_configured_address(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err("SYNTHCHAT_BACKEND_ADDR must contain valid Unicode".to_owned())
        }
    }
}

fn configured_development_origin() -> Result<Option<String>, String> {
    if !cfg!(debug_assertions) || env::var_os("SYNTHCHAT_ALLOWED_ORIGINS").is_some() {
        return Ok(None);
    }
    match env::var("SYNTHCHAT_DESKTOP_DEV_ORIGIN") {
        Ok(origin) => Ok(Some(origin)),
        Err(env::VarError::NotPresent) => Ok(Some(DEVELOPMENT_ORIGIN.to_owned())),
        Err(env::VarError::NotUnicode(_)) => {
            Err("SYNTHCHAT_DESKTOP_DEV_ORIGIN must contain valid Unicode".to_owned())
        }
    }
}

fn parse_configured_address(value: Option<&str>) -> Result<SocketAddr, String> {
    let value = value.unwrap_or(DEFAULT_BACKEND_ADDR);
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| "SYNTHCHAT_BACKEND_ADDR must be an IP socket address".to_owned())?;
    let is_supported_loopback = match address.ip() {
        IpAddr::V4(ip) => ip == Ipv4Addr::LOCALHOST,
        IpAddr::V6(ip) => ip == Ipv6Addr::LOCALHOST,
    };
    if !is_supported_loopback {
        return Err("SYNTHCHAT_BACKEND_ADDR must use 127.0.0.1 or ::1".to_owned());
    }
    Ok(address)
}

fn backend_base_url(address: SocketAddr) -> String {
    format!("http://{address}")
}

fn backend_binary() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("SYNTHCHAT_BACKEND_BINARY") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("SYNTHCHAT_BACKEND_BINARY must be an absolute path".to_owned());
        }
        return Ok(path);
    }

    let mut path = env::current_exe()
        .map_err(|_| "failed to locate the desktop executable directory".to_owned())?;
    path.set_file_name(if cfg!(windows) {
        "synthchat-hermes-backend.exe"
    } else {
        "synthchat-hermes-backend"
    });
    Ok(path)
}

struct SpawnedBackend {
    child: Child,
    stdin: ChildStdin,
    address: SocketAddr,
    diagnostics: StartupDiagnostics,
}

fn launch_real_backend(
    config: &BackendLaunchConfig,
    runtime: &DesktopRuntimeConfig,
    token: &str,
) -> Result<LaunchedBackend, LaunchFailure> {
    let deadline = Instant::now() + runtime.startup_timeout;
    let spawned = spawn_backend(config, runtime, token, deadline)?;
    let mut backend = RealManagedBackend {
        child: spawned.child,
        stdin: Some(spawned.stdin),
    };
    if let Err(error) = wait_until_ready(
        &mut backend.child,
        spawned.address,
        token,
        runtime,
        deadline,
    ) {
        let stop_result = backend.stop(runtime);
        let message = append_startup_diagnostic(
            error,
            spawned.diagnostics,
            token,
            runtime.diagnostic_timeout,
            runtime.stderr_max_bytes,
        );
        return match stop_result {
            Ok(()) => Err(LaunchFailure::retryable(message)),
            Err(stop_error) => Err(LaunchFailure::unsafe_to_retry(format!(
                "{message}; backend termination could not be confirmed: {stop_error}; automatic restart disabled"
            ))),
        };
    }
    Ok(LaunchedBackend {
        child: Box::new(backend),
        address: spawned.address,
    })
}

fn spawn_backend(
    config: &BackendLaunchConfig,
    runtime: &DesktopRuntimeConfig,
    token: &str,
    deadline: Instant,
) -> Result<SpawnedBackend, LaunchFailure> {
    let mut command = Command::new(&config.binary);
    command
        .env_remove("SYNTHCHAT_DESKTOP_TOKEN")
        .env(
            "SYNTHCHAT_BACKEND_ADDR",
            config.configured_address.to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(development_origin) = config.development_origin.as_ref() {
        command.env("SYNTHCHAT_ALLOWED_ORIGINS", development_origin);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    let mut child = command
        .spawn()
        .map_err(|_| LaunchFailure::retryable("failed to start the backend process"))?;
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            return Err(cleanup_startup_failure(
                child,
                None,
                None,
                "backend startup diagnostic pipe is unavailable".to_owned(),
                runtime,
                token,
            ));
        }
    };
    let diagnostics = match StartupDiagnostics::start(stderr, runtime.stderr_max_bytes) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            return Err(cleanup_startup_failure(
                child, None, None, error, runtime, token,
            ));
        }
    };
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            return Err(cleanup_startup_failure(
                child,
                None,
                Some(diagnostics),
                "backend token pipe is unavailable".to_owned(),
                runtime,
                token,
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            return Err(cleanup_startup_failure(
                child,
                Some(stdin),
                Some(diagnostics),
                "backend startup handshake pipe is unavailable".to_owned(),
                runtime,
                token,
            ));
        }
    };
    if stdin
        .write_all(token.as_bytes())
        .and_then(|_| stdin.write_all(b"\n"))
        .is_err()
    {
        return Err(cleanup_startup_failure(
            child,
            Some(stdin),
            Some(diagnostics),
            "failed to send the backend session token".to_owned(),
            runtime,
            token,
        ));
    }
    let address = match wait_for_startup_handshake(
        &mut child,
        stdout,
        config.configured_address,
        runtime,
        deadline,
    ) {
        Ok(address) => address,
        Err(error) => {
            return Err(cleanup_startup_failure(
                child,
                Some(stdin),
                Some(diagnostics),
                error,
                runtime,
                token,
            ));
        }
    };
    Ok(SpawnedBackend {
        child,
        stdin,
        address,
        diagnostics,
    })
}

fn cleanup_startup_failure(
    mut child: Child,
    stdin: Option<ChildStdin>,
    diagnostics: Option<StartupDiagnostics>,
    error: String,
    runtime: &DesktopRuntimeConfig,
    token: &str,
) -> LaunchFailure {
    let stop_result = stop_child(&mut child, stdin, runtime);
    let message = diagnostics.map_or(error.clone(), |diagnostics| {
        append_startup_diagnostic(
            error,
            diagnostics,
            token,
            runtime.diagnostic_timeout,
            runtime.stderr_max_bytes,
        )
    });
    match stop_result {
        Ok(()) => LaunchFailure::retryable(message),
        Err(stop_error) => LaunchFailure::unsafe_to_retry(format!(
            "{message}; backend termination could not be confirmed: {stop_error}; automatic restart disabled"
        )),
    }
}

struct StartupDiagnostics {
    receiver: mpsc::Receiver<Vec<u8>>,
}

impl StartupDiagnostics {
    fn start(stderr: ChildStderr, max_bytes: usize) -> Result<Self, String> {
        Self::start_reader(stderr, max_bytes)
    }

    fn start_reader(
        mut reader: impl Read + Send + 'static,
        max_bytes: usize,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("synthchat-backend-stderr".to_owned())
            .spawn(move || {
                let mut captured = Vec::with_capacity(max_bytes);
                let mut buffer = [0_u8; 512];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            let remaining = max_bytes.saturating_sub(captured.len());
                            let take = remaining.min(read);
                            captured.extend_from_slice(&buffer[..take]);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = sender.send(captured);
            })
            .map_err(|_| "failed to start the backend diagnostic reader".to_owned())?;
        Ok(Self { receiver })
    }

    fn summary(self, token: &str, timeout: Duration, max_bytes: usize) -> Option<String> {
        let bytes = self.receiver.recv_timeout(timeout).ok()?;
        sanitize_startup_diagnostic(&bytes, token, max_bytes)
    }
}

fn append_startup_diagnostic(
    error: String,
    diagnostics: StartupDiagnostics,
    token: &str,
    timeout: Duration,
    max_bytes: usize,
) -> String {
    match diagnostics.summary(token, timeout, max_bytes) {
        Some(summary) => format!("{error}; backend startup stderr: {summary}"),
        None => error,
    }
}

fn sanitize_startup_diagnostic(bytes: &[u8], token: &str, max_bytes: usize) -> Option<String> {
    let raw = String::from_utf8_lossy(bytes);
    let variables = extract_synthchat_variables(&raw);
    let replaced = raw.replace(token, "[redacted]");
    let normalized = replaced
        .chars()
        .map(|character| {
            if character.is_ascii_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();

    let mut sanitized = Vec::new();
    let mut redact_next = 0_usize;
    for word in normalized.split_whitespace() {
        if redact_next > 0 {
            sanitized.push("[redacted]".to_owned());
            redact_next -= 1;
            continue;
        }
        let upper = word.to_ascii_uppercase();
        let key = upper
            .split_once('=')
            .map(|(key, _)| key)
            .unwrap_or(upper.trim_end_matches(':'));
        if contains_sensitive_label(key) {
            if let Some((label, _)) = word.split_once('=') {
                sanitized.push(format!("{label}=[redacted]"));
            } else {
                sanitized.push(word.to_owned());
                redact_next = 2;
            }
        } else if word.contains("://") && (word.contains('?') || word.contains('@')) {
            sanitized.push("[redacted-url]".to_owned());
        } else if looks_like_opaque_secret(word) {
            sanitized.push("[redacted]".to_owned());
        } else {
            sanitized.push(word.to_owned());
        }
    }

    let mut summary = sanitized.join(" ");
    if !variables.is_empty() {
        summary = format!(
            "configuration variable(s) [{}]: {summary}",
            variables.join(", ")
        );
    }
    let summary = truncate_utf8(&summary, max_bytes);
    (!summary.is_empty()).then_some(summary)
}

fn contains_sensitive_label(value: &str) -> bool {
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "AUTHORIZATION",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

fn looks_like_opaque_secret(word: &str) -> bool {
    let candidate = word.trim_matches(|character: char| {
        matches!(character, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')
    });
    candidate.len() >= 24
        && !candidate.starts_with("SYNTHCHAT_")
        && candidate.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'+' | b'=' | b'.')
        })
}

fn extract_synthchat_variables(raw: &str) -> Vec<String> {
    let bytes = raw.as_bytes();
    let mut variables = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"SYNTHCHAT_") {
            let start = index;
            index += "SYNTHCHAT_".len();
            while index < bytes.len()
                && (bytes[index].is_ascii_uppercase()
                    || bytes[index].is_ascii_digit()
                    || bytes[index] == b'_')
            {
                index += 1;
            }
            let variable = &raw[start..index];
            if variable.len() <= 128
                && !variables.iter().any(|existing| existing == variable)
                && variables.len() < 8
            {
                variables.push(variable.to_owned());
            }
        } else {
            index += 1;
        }
    }
    variables
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn wait_for_startup_handshake(
    child: &mut Child,
    stdout: ChildStdout,
    configured_address: SocketAddr,
    runtime: &DesktopRuntimeConfig,
    deadline: Instant,
) -> Result<SocketAddr, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("synthchat-backend-stdout".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let result = read_startup_handshake(&mut reader, configured_address);
            let _ = sender.send(result);
            let _ = io::copy(&mut reader, &mut io::sink());
        })
        .map_err(|_| "failed to start the backend handshake reader".to_owned())?;

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(
                "backend did not report its listening address before the startup deadline"
                    .to_owned(),
            );
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(runtime.process_poll_interval);
        match receiver.recv_timeout(wait) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("backend startup handshake pipe closed unexpectedly".to_owned());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if child
            .try_wait()
            .map_err(|_| "failed to inspect the backend process".to_owned())?
            .is_some()
        {
            return Err("backend exited before reporting its listening address".to_owned());
        }
    }
}

fn read_startup_handshake(
    reader: &mut impl BufRead,
    configured_address: SocketAddr,
) -> Result<SocketAddr, String> {
    let mut bytes = Vec::with_capacity(MAX_STARTUP_HANDSHAKE_BYTES);
    let bytes_read = reader
        .take((MAX_STARTUP_HANDSHAKE_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "failed to read the backend startup handshake".to_owned())?;
    if bytes_read == 0 || bytes.len() > MAX_STARTUP_HANDSHAKE_BYTES {
        return Err("backend startup handshake is missing or too long".to_owned());
    }
    let line = std::str::from_utf8(&bytes)
        .map_err(|_| "backend startup handshake must contain valid UTF-8".to_owned())?;
    parse_startup_handshake(line, configured_address)
}

fn parse_startup_handshake(
    line: &str,
    configured_address: SocketAddr,
) -> Result<SocketAddr, String> {
    let line = line
        .strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .ok_or_else(|| "backend startup handshake is not line terminated".to_owned())?;
    let value = line
        .strip_prefix(STARTUP_HANDSHAKE_PREFIX)
        .ok_or_else(|| "backend startup handshake has an invalid protocol marker".to_owned())?;
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| "backend startup handshake has an invalid socket address".to_owned())?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("backend startup handshake must report a bound loopback address".to_owned());
    }
    let matches_configuration = if configured_address.port() == 0 {
        address.ip() == configured_address.ip()
    } else {
        address == configured_address
    };
    if !matches_configuration {
        return Err("backend startup handshake does not match the configured address".to_owned());
    }
    Ok(address)
}

fn wait_until_ready(
    child: &mut Child,
    address: SocketAddr,
    token: &str,
    runtime: &DesktopRuntimeConfig,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        if Instant::now() >= deadline {
            return Err("backend did not become ready before the startup deadline".to_owned());
        }
        if child
            .try_wait()
            .map_err(|_| "failed to inspect the backend process".to_owned())?
            .is_some()
        {
            return Err("backend exited before becoming ready".to_owned());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let probe_timeout = runtime.probe_timeout.min(remaining);
        if !probe_timeout.is_zero() && probe_authenticated(address, token, probe_timeout) {
            if child
                .try_wait()
                .map_err(|_| "failed to inspect the backend process".to_owned())?
                .is_none()
            {
                return Ok(());
            }
            return Err("backend exited during the readiness handshake".to_owned());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            thread::sleep(remaining.min(runtime.process_poll_interval));
        }
    }
}

fn probe_authenticated(address: SocketAddr, token: &str, timeout: Duration) -> bool {
    probe_endpoint(
        address,
        "/api/v1/capabilities",
        Some(token),
        b"\"contractVersion\":\"v1\"",
        timeout,
    )
}

fn probe_endpoint(
    address: SocketAddr,
    path: &str,
    token: Option<&str>,
    expected_body: &[u8],
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return false;
    };
    let authorization = token
        .map(|value| format!("Authorization: Bearer {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}Connection: close\r\n\r\n"
    );
    if !write_all_before(&mut stream, request.as_bytes(), deadline) {
        return false;
    }

    let mut response = Vec::with_capacity(512);
    let mut buffer = [0_u8; 512];
    while response.len() < MAX_PROBE_RESPONSE_BYTES {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        let capacity = (MAX_PROBE_RESPONSE_BYTES - response.len()).min(buffer.len());
        match stream.read(&mut buffer[..capacity]) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if response_matches(&response, expected_body) {
                    return true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    response_matches(&response, expected_body)
}

fn write_all_before(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> bool {
    while !bytes.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        if remaining.is_zero() || stream.set_write_timeout(Some(remaining)).is_err() {
            return false;
        }
        match stream.write(bytes) {
            Ok(0) => return false,
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

fn response_matches(response: &[u8], expected_body: &[u8]) -> bool {
    response.starts_with(b"HTTP/1.1 200")
        && response
            .windows(expected_body.len())
            .any(|window| window == expected_body)
}

fn stop_child(
    child: &mut Child,
    stdin: Option<ChildStdin>,
    runtime: &DesktopRuntimeConfig,
) -> Result<(), String> {
    drop(stdin);
    if wait_for_exit(
        child,
        runtime.shutdown_grace_timeout,
        runtime.process_poll_interval,
    )? {
        return Ok(());
    }

    let kill_result = child.kill();
    if wait_for_exit(
        child,
        runtime.termination_timeout,
        runtime.process_poll_interval,
    )? {
        return Ok(());
    }
    if kill_result.is_err() {
        return Err("failed to terminate the backend process".to_owned());
    }
    Err("backend process did not exit before the termination deadline".to_owned())
}

fn wait_for_exit(
    child: &mut Child,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(true),
            Err(_) => {
                return Err("failed to inspect the backend process during shutdown".to_owned());
            }
            Ok(None) if Instant::now() >= deadline => return Ok(false),
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    thread::sleep(remaining.min(poll_interval));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::Cursor,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
    };

    use super::*;

    const TEST_WAIT: Duration = Duration::from_secs(2);

    #[derive(Clone)]
    struct FakeClock {
        now: Arc<Mutex<Instant>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Arc::new(Mutex::new(Instant::now())),
            }
        }

        fn advance(&self, duration: Duration) {
            let mut now = self.now.lock().unwrap();
            *now += duration;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
    }

    enum FakeChildState {
        Running,
        Exited(String),
    }

    struct FakeChildControl {
        state: Mutex<FakeChildState>,
        stop_started: Mutex<Option<mpsc::Sender<()>>>,
        stop_release: Mutex<Option<mpsc::Receiver<()>>>,
        stop_error: Mutex<Option<String>>,
        stops: AtomicUsize,
    }

    struct FakeChild {
        control: Arc<FakeChildControl>,
    }

    impl ManagedBackend for FakeChild {
        fn try_wait(&mut self) -> Result<Option<String>, String> {
            match &*self.control.state.lock().unwrap() {
                FakeChildState::Running => Ok(None),
                FakeChildState::Exited(status) => Ok(Some(status.clone())),
            }
        }

        fn stop(&mut self, _runtime: &DesktopRuntimeConfig) -> Result<(), String> {
            self.control.stops.fetch_add(1, Ordering::SeqCst);
            if let Some(sender) = self.control.stop_started.lock().unwrap().take() {
                let _ = sender.send(());
            }
            if let Some(receiver) = self.control.stop_release.lock().unwrap().take() {
                let _ = receiver.recv();
            }
            match self.control.stop_error.lock().unwrap().take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    struct FakeLauncher {
        outcomes: Mutex<VecDeque<Result<SocketAddr, String>>>,
        probes: Mutex<VecDeque<bool>>,
        tokens: Mutex<Vec<String>>,
        children: Mutex<Vec<Arc<FakeChildControl>>>,
        launches: AtomicUsize,
        launch_started: Mutex<Option<mpsc::Sender<()>>>,
        launch_release: Mutex<Option<mpsc::Receiver<()>>>,
        probe_observer: Mutex<Option<mpsc::Sender<()>>>,
    }

    impl FakeLauncher {
        fn new(outcomes: impl IntoIterator<Item = Result<SocketAddr, String>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
                probes: Mutex::new(VecDeque::new()),
                tokens: Mutex::new(Vec::new()),
                children: Mutex::new(Vec::new()),
                launches: AtomicUsize::new(0),
                launch_started: Mutex::new(None),
                launch_release: Mutex::new(None),
                probe_observer: Mutex::new(None),
            }
        }

        fn block_next_launch(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
            let (started_sender, started_receiver) = mpsc::channel();
            let (release_sender, release_receiver) = mpsc::channel();
            *self.launch_started.lock().unwrap() = Some(started_sender);
            *self.launch_release.lock().unwrap() = Some(release_receiver);
            (started_receiver, release_sender)
        }

        fn block_latest_stop(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
            let (started_sender, started_receiver) = mpsc::channel();
            let (release_sender, release_receiver) = mpsc::channel();
            let child = self.children.lock().unwrap().last().unwrap().clone();
            *child.stop_started.lock().unwrap() = Some(started_sender);
            *child.stop_release.lock().unwrap() = Some(release_receiver);
            (started_receiver, release_sender)
        }

        fn observe_next_probe(&self) -> mpsc::Receiver<()> {
            let (sender, receiver) = mpsc::channel();
            *self.probe_observer.lock().unwrap() = Some(sender);
            receiver
        }

        fn launch_count(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }

        fn tokens(&self) -> Vec<String> {
            self.tokens.lock().unwrap().clone()
        }

        fn exit_latest(&self, status: &str) {
            let child = self.children.lock().unwrap().last().unwrap().clone();
            *child.state.lock().unwrap() = FakeChildState::Exited(status.to_owned());
        }

        fn latest_stop_count(&self) -> usize {
            self.children
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .stops
                .load(Ordering::SeqCst)
        }

        fn fail_latest_stop(&self, error: &str) {
            let child = self.children.lock().unwrap().last().unwrap().clone();
            *child.stop_error.lock().unwrap() = Some(error.to_owned());
        }
    }

    impl BackendLauncher for FakeLauncher {
        fn launch_ready(
            &self,
            _config: &BackendLaunchConfig,
            _runtime: &DesktopRuntimeConfig,
            token: &str,
        ) -> Result<LaunchedBackend, LaunchFailure> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            self.tokens.lock().unwrap().push(token.to_owned());
            if let Some(sender) = self.launch_started.lock().unwrap().take() {
                let _ = sender.send(());
            }
            if let Some(receiver) = self.launch_release.lock().unwrap().take() {
                let _ = receiver.recv();
            }
            let address = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("unexpected launch attempt".to_owned()))
                .map_err(LaunchFailure::retryable)?;
            let control = Arc::new(FakeChildControl {
                state: Mutex::new(FakeChildState::Running),
                stop_started: Mutex::new(None),
                stop_release: Mutex::new(None),
                stop_error: Mutex::new(None),
                stops: AtomicUsize::new(0),
            });
            self.children.lock().unwrap().push(control.clone());
            Ok(LaunchedBackend {
                child: Box::new(FakeChild { control }),
                address,
            })
        }

        fn probe_authenticated(
            &self,
            _address: SocketAddr,
            _token: &str,
            _runtime: &DesktopRuntimeConfig,
        ) -> bool {
            if let Some(observer) = self.probe_observer.lock().unwrap().take() {
                let _ = observer.send(());
            }
            self.probes.lock().unwrap().pop_front().unwrap_or(true)
        }
    }

    struct FakeTokenSource {
        tokens: Mutex<VecDeque<String>>,
    }

    impl FakeTokenSource {
        fn new(tokens: impl IntoIterator<Item = String>) -> Self {
            Self {
                tokens: Mutex::new(tokens.into_iter().collect()),
            }
        }
    }

    impl TokenSource for FakeTokenSource {
        fn generate(&self) -> Result<String, String> {
            self.tokens
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake token source exhausted".to_owned())
        }
    }

    fn fake_address(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    fn test_runtime() -> DesktopRuntimeConfig {
        DesktopRuntimeConfig {
            startup_timeout: Duration::from_secs(1),
            probe_timeout: Duration::from_millis(25),
            monitor_interval: Duration::from_secs(60),
            process_poll_interval: Duration::from_millis(1),
            shutdown_grace_timeout: Duration::from_millis(25),
            termination_timeout: Duration::from_millis(25),
            restart_backoff_initial: Duration::from_millis(100),
            restart_backoff_max: Duration::from_millis(400),
            stable_window: Duration::from_secs(1),
            diagnostic_timeout: Duration::from_millis(25),
            stderr_max_bytes: 512,
        }
    }

    fn test_launch_config() -> BackendLaunchConfig {
        BackendLaunchConfig {
            binary: PathBuf::from("fake-backend"),
            configured_address: "127.0.0.1:0".parse().unwrap(),
            development_origin: None,
        }
    }

    fn test_manager(
        launcher: Arc<FakeLauncher>,
        clock: Arc<FakeClock>,
        token_source: Arc<FakeTokenSource>,
    ) -> BackendManager {
        BackendManager::start_with_dependencies(
            test_launch_config(),
            test_runtime(),
            launcher,
            clock,
            token_source,
        )
    }

    fn wait_for_state(
        manager: &BackendManager,
        expected: BackendLifecycleState,
    ) -> BackendSnapshot {
        let current = manager.snapshots.current.lock().unwrap();
        let (current, timeout) = manager
            .snapshots
            .changed
            .wait_timeout_while(current, TEST_WAIT, |snapshot| snapshot.state != expected)
            .unwrap();
        assert!(!timeout.timed_out(), "timed out waiting for {expected:?}");
        current.clone()
    }

    #[test]
    fn generated_token_has_256_bits_encoded_as_visible_ascii() {
        let token = generate_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn operating_system_randomness_failure_is_an_explicit_startup_error() {
        assert_eq!(
            generate_token_with(|_| Err(())).unwrap_err(),
            "operating-system randomness is unavailable"
        );
    }

    #[test]
    fn backend_address_defaults_to_an_os_assigned_loopback_port() {
        assert_eq!(
            parse_configured_address(None).unwrap(),
            "127.0.0.1:0".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            parse_configured_address(Some("[::1]:9123")).unwrap(),
            "[::1]:9123".parse::<SocketAddr>().unwrap()
        );
        assert!(parse_configured_address(Some("0.0.0.0:9123")).is_err());
        assert!(parse_configured_address(Some("127.0.0.2:9123")).is_err());
    }

    #[test]
    fn startup_handshake_returns_the_actual_loopback_address() {
        let configured = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
        let actual =
            parse_startup_handshake("SYNTHCHAT_BACKEND_READY 127.0.0.1:49152\r\n", configured)
                .unwrap();

        assert_eq!(actual, "127.0.0.1:49152".parse().unwrap());
        assert_eq!(backend_base_url(actual), "http://127.0.0.1:49152");
    }

    #[test]
    fn startup_handshake_rejects_zero_remote_or_mismatched_addresses() {
        let dynamic = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
        assert!(parse_startup_handshake("SYNTHCHAT_BACKEND_READY 127.0.0.1:0\n", dynamic).is_err());
        assert!(
            parse_startup_handshake("SYNTHCHAT_BACKEND_READY 192.0.2.1:9000\n", dynamic).is_err()
        );

        let fixed = "127.0.0.1:9123".parse::<SocketAddr>().unwrap();
        assert!(
            parse_startup_handshake("SYNTHCHAT_BACKEND_READY 127.0.0.1:9124\n", fixed).is_err()
        );
    }

    #[test]
    fn runtime_tuning_has_safe_defaults_and_accepts_bounded_overrides() {
        let defaults = DesktopRuntimeConfig::from_lookup(|_| Ok(None)).unwrap();
        assert_eq!(defaults.startup_timeout, Duration::from_secs(8));
        assert_eq!(defaults.monitor_interval, Duration::from_millis(500));
        assert_eq!(defaults.process_poll_interval, Duration::from_millis(20));
        assert_eq!(defaults.stable_window, Duration::from_secs(30));
        assert_eq!(defaults.stderr_max_bytes, 4_096);

        let values = [
            ("SYNTHCHAT_DESKTOP_STARTUP_TIMEOUT_MS", "9000"),
            ("SYNTHCHAT_DESKTOP_PROBE_TIMEOUT_MS", "300"),
            ("SYNTHCHAT_DESKTOP_MONITOR_INTERVAL_MS", "750"),
            ("SYNTHCHAT_DESKTOP_PROCESS_POLL_INTERVAL_MS", "10"),
            ("SYNTHCHAT_DESKTOP_SHUTDOWN_GRACE_TIMEOUT_MS", "2500"),
            ("SYNTHCHAT_DESKTOP_TERMINATION_TIMEOUT_MS", "1500"),
            ("SYNTHCHAT_DESKTOP_RESTART_BACKOFF_INITIAL_MS", "300"),
            ("SYNTHCHAT_DESKTOP_RESTART_BACKOFF_MAX_MS", "9000"),
            ("SYNTHCHAT_DESKTOP_STABLE_WINDOW_MS", "45000"),
            ("SYNTHCHAT_DESKTOP_DIAGNOSTIC_TIMEOUT_MS", "400"),
            ("SYNTHCHAT_DESKTOP_STDERR_MAX_BYTES", "8192"),
        ];
        let configured = DesktopRuntimeConfig::from_lookup(|name| {
            Ok(values
                .iter()
                .find_map(|(key, value)| (*key == name).then(|| (*value).to_owned())))
        })
        .unwrap();
        assert_eq!(configured.monitor_interval, Duration::from_millis(750));
        assert_eq!(configured.stable_window, Duration::from_secs(45));
        assert_eq!(configured.stderr_max_bytes, 8_192);
    }

    #[test]
    fn runtime_tuning_rejects_invalid_ranges_and_backoff_order() {
        let invalid_number = DesktopRuntimeConfig::from_lookup(|name| {
            Ok((name == "SYNTHCHAT_DESKTOP_PROBE_TIMEOUT_MS").then(|| "fast".to_owned()))
        })
        .unwrap_err();
        assert!(invalid_number.contains("SYNTHCHAT_DESKTOP_PROBE_TIMEOUT_MS"));

        let invalid_size = DesktopRuntimeConfig::from_lookup(|name| {
            Ok((name == "SYNTHCHAT_DESKTOP_STDERR_MAX_BYTES").then(|| "64".to_owned()))
        })
        .unwrap_err();
        assert!(invalid_size.contains("between 256 and 65536"));

        let invalid_order = DesktopRuntimeConfig::from_lookup(|name| match name {
            "SYNTHCHAT_DESKTOP_RESTART_BACKOFF_INITIAL_MS" => Ok(Some("500".to_owned())),
            "SYNTHCHAT_DESKTOP_RESTART_BACKOFF_MAX_MS" => Ok(Some("400".to_owned())),
            _ => Ok(None),
        })
        .unwrap_err();
        assert!(invalid_order.contains("must not exceed"));
    }

    #[test]
    fn manager_start_and_ipc_snapshots_do_not_wait_for_blocked_launch() {
        let launcher = Arc::new(FakeLauncher::new([Ok(fake_address(41001))]));
        let (launch_started, launch_release) = launcher.block_next_launch();
        let manager = test_manager(
            launcher,
            Arc::new(FakeClock::new()),
            Arc::new(FakeTokenSource::new(["a".repeat(64)])),
        );
        launch_started.recv_timeout(TEST_WAIT).unwrap();

        let status = manager.status();
        assert_eq!(status.state, BackendLifecycleState::Starting);
        assert!(manager.connection().is_err());

        launch_release.send(()).unwrap();
        let ready = wait_for_state(&manager, BackendLifecycleState::Ready);
        assert_eq!(ready.generation, Some(1));
    }

    #[test]
    fn stopping_generation_blocks_replacement_until_stop_is_confirmed() {
        let launcher = Arc::new(FakeLauncher::new([
            Ok(fake_address(42001)),
            Ok(fake_address(42002)),
        ]));
        let clock = Arc::new(FakeClock::new());
        let manager = test_manager(
            launcher.clone(),
            clock.clone(),
            Arc::new(FakeTokenSource::new(["b".repeat(64), "c".repeat(64)])),
        );
        wait_for_state(&manager, BackendLifecycleState::Ready);
        let (stop_started, stop_release) = launcher.block_latest_stop();
        launcher.exit_latest("exit code 17");
        manager.wake_worker();
        stop_started.recv_timeout(TEST_WAIT).unwrap();

        let stopping = manager.status();
        assert_eq!(stopping.state, BackendLifecycleState::Stopping);
        assert!(manager.connection().is_err());
        assert_eq!(launcher.launch_count(), 1);
        clock.advance(Duration::from_secs(10));
        manager.wake_worker();
        assert_eq!(launcher.launch_count(), 1);

        stop_release.send(()).unwrap();
        let backoff = wait_for_state(&manager, BackendLifecycleState::Backoff);
        assert!(backoff.error.unwrap().contains("100 ms"));
        assert_eq!(launcher.launch_count(), 1);
    }

    #[test]
    fn unconfirmed_stop_enters_terminal_failure_without_restarting() {
        let launcher = Arc::new(FakeLauncher::new([
            Ok(fake_address(42501)),
            Ok(fake_address(42502)),
        ]));
        let clock = Arc::new(FakeClock::new());
        let manager = test_manager(
            launcher.clone(),
            clock.clone(),
            Arc::new(FakeTokenSource::new(["7".repeat(64), "8".repeat(64)])),
        );
        wait_for_state(&manager, BackendLifecycleState::Ready);
        launcher.fail_latest_stop("process still running");
        launcher.exit_latest("probe failed");
        manager.wake_worker();

        let failed = wait_for_state(&manager, BackendLifecycleState::Failed);
        assert!(failed.error.unwrap().contains("automatic restart disabled"));
        clock.advance(Duration::from_secs(10));
        manager.wake_worker();
        assert_eq!(launcher.launch_count(), 1);
    }

    #[test]
    fn quick_ready_crashes_keep_increasing_restart_backoff() {
        let launcher = Arc::new(FakeLauncher::new([
            Ok(fake_address(43001)),
            Ok(fake_address(43002)),
            Ok(fake_address(43003)),
        ]));
        let clock = Arc::new(FakeClock::new());
        let manager = test_manager(
            launcher.clone(),
            clock.clone(),
            Arc::new(FakeTokenSource::new([
                "d".repeat(64),
                "e".repeat(64),
                "f".repeat(64),
            ])),
        );
        wait_for_state(&manager, BackendLifecycleState::Ready);

        launcher.exit_latest("first quick crash");
        manager.wake_worker();
        let first = wait_for_state(&manager, BackendLifecycleState::Backoff);
        assert!(first.error.unwrap().contains("100 ms"));
        clock.advance(Duration::from_millis(100));
        manager.wake_worker();
        wait_for_state(&manager, BackendLifecycleState::Ready);

        launcher.exit_latest("second quick crash");
        clock.advance(Duration::from_secs(2));
        manager.wake_worker();
        let second = wait_for_state(&manager, BackendLifecycleState::Backoff);
        assert!(second.error.unwrap().contains("200 ms"));
        assert_eq!(launcher.launch_count(), 2);
        assert_eq!(launcher.tokens(), vec!["d".repeat(64), "e".repeat(64)]);
    }

    #[test]
    fn stable_generation_resets_failure_count_before_a_later_crash() {
        let launcher = Arc::new(FakeLauncher::new([
            Err("initial failure".to_owned()),
            Ok(fake_address(44001)),
        ]));
        let clock = Arc::new(FakeClock::new());
        let manager = test_manager(
            launcher.clone(),
            clock.clone(),
            Arc::new(FakeTokenSource::new(["1".repeat(64), "2".repeat(64)])),
        );
        wait_for_state(&manager, BackendLifecycleState::Backoff);
        clock.advance(Duration::from_millis(100));
        manager.wake_worker();
        wait_for_state(&manager, BackendLifecycleState::Ready);

        clock.advance(Duration::from_secs(1));
        let stable_probe = launcher.observe_next_probe();
        manager.wake_worker();
        stable_probe.recv_timeout(TEST_WAIT).unwrap();
        launcher.exit_latest("crash after stable window");
        manager.wake_worker();
        let backoff = wait_for_state(&manager, BackendLifecycleState::Backoff);
        assert!(backoff.error.unwrap().contains("100 ms"));
    }

    #[test]
    fn startup_stderr_is_bounded_and_redacted_but_names_invalid_skill_variable() {
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let raw = format!(
            "invalid SYNTHCHAT_SKILL_REGISTRY_INDEX_URL=https://user:pass@example.test/?key=secret Authorization: Bearer short-secret api_key=tiny {secret} trailing"
        );
        let diagnostic = sanitize_startup_diagnostic(raw.as_bytes(), secret, 256).unwrap();

        assert!(diagnostic.contains("SYNTHCHAT_SKILL_REGISTRY_INDEX_URL"));
        assert!(!diagnostic.contains(secret));
        assert!(!diagnostic.contains("short-secret"));
        assert!(!diagnostic.contains("api_key=tiny"));
        assert!(!diagnostic.contains("user:pass"));
        assert!(diagnostic.len() <= 256);
    }

    #[test]
    fn stderr_reader_captures_only_the_configured_prefix_and_drains_to_eof() {
        let diagnostic = StartupDiagnostics::start_reader(
            Cursor::new(b"SYNTHCHAT_SKILL_GITHUB_API_BASE_URL invalid more-data".repeat(100)),
            64,
        )
        .unwrap()
        .summary("a-secret-token", TEST_WAIT, 64)
        .unwrap();

        assert!(diagnostic.contains("SYNTHCHAT_SKILL_GITHUB_API_BASE_URL"));
        assert!(diagnostic.len() <= 64);
    }

    #[test]
    fn drop_notifies_and_joins_worker_without_starting_another_generation() {
        let launcher = Arc::new(FakeLauncher::new([
            Ok(fake_address(45001)),
            Ok(fake_address(45002)),
        ]));
        let manager = test_manager(
            launcher.clone(),
            Arc::new(FakeClock::new()),
            Arc::new(FakeTokenSource::new(["3".repeat(64), "4".repeat(64)])),
        );
        wait_for_state(&manager, BackendLifecycleState::Ready);

        drop(manager);
        assert_eq!(launcher.launch_count(), 1);
        assert_eq!(launcher.latest_stop_count(), 1);
    }

    #[test]
    fn real_sidecar_completes_the_authenticated_desktop_lifecycle_when_requested() {
        let Some(binary) = env::var_os("SYNTHCHAT_DESKTOP_SIDECAR_SMOKE_BINARY") else {
            return;
        };
        let binary = PathBuf::from(binary);
        assert!(binary.is_absolute() && binary.is_file());
        let manager = BackendManager::start_with_dependencies(
            BackendLaunchConfig {
                binary,
                configured_address: "127.0.0.1:0".parse().unwrap(),
                development_origin: None,
            },
            DesktopRuntimeConfig::default(),
            Arc::new(RealBackendLauncher),
            Arc::new(SystemClock),
            Arc::new(OsTokenSource),
        );

        wait_for_state(&manager, BackendLifecycleState::Ready);
        let connection = manager.connection().unwrap();
        let address = connection
            .base_url
            .strip_prefix("http://")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
        assert_eq!(connection.token.len(), 64);
        drop(manager);
    }
}
