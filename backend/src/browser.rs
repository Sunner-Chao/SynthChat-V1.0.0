use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions as CapOpenOptions},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use tempfile::{Builder as TempBuilder, TempDir};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, lookup_host},
    process::{Child, Command},
    sync::{Mutex, Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use url::Url;
use uuid::Uuid;

use crate::{
    processes::platform,
    tools::{ToolExecutionControl, ToolExecutionControlError},
};

const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(20);
const BROWSER_NAVIGATION_TIMEOUT: Duration = Duration::from_secs(30);
const BROWSER_PROFILE_CLEANUP_ATTEMPTS: usize = 80;
const BROWSER_PROFILE_CLEANUP_INTERVAL: Duration = Duration::from_millis(25);
const CDP_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const CDP_POLL_INTERVAL: Duration = Duration::from_millis(75);
const DNS_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);
const PROXY_HEADER_LIMIT: usize = 32 * 1024;
const PROXY_CONNECTION_LIMIT: usize = 32;
const MAX_AX_NODES: usize = 220;
const MAX_AX_TEXT_CHARS: usize = 280;
const MAX_SNAPSHOT_BYTES: usize = 48 * 1024;
const MAX_CONSOLE_ENTRIES: usize = 80;
const MAX_CONSOLE_TEXT_CHARS: usize = 1_500;
const MAX_IMAGE_BASE64_CHARS: usize = 44 * 1024;
const MAX_SCREENSHOT_WIDTH: f64 = 640.0;
const MAX_SCREENSHOT_HEIGHT: f64 = 480.0;
const DOWNLOAD_DIRECTORY_NAME: &str = "downloads";
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RUN_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RUN_DOWNLOADS: usize = 4;
const MAX_DOWNLOAD_FILENAME_CHARS: usize = 128;
const MAX_DOWNLOAD_DIRECTORY_ENTRIES: usize = 8;
const MAX_DOWNLOAD_SCAN_BYTES: usize = 64 * 1024;
const MAX_DOWNLOAD_WAIT: Duration = Duration::from_secs(30);
const MAX_RECOVERY_TREE_DEPTH: usize = 6;
const MAX_RECOVERY_TREE_ENTRIES: usize = 128;

/// A browser is deliberately scoped to exactly one Profile/Session/Run. The
/// values are never used as path components; a per-session temp directory is
/// generated below instead.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BrowserOwner {
    profile_id: String,
    session_id: String,
    run_id: String,
}

impl BrowserOwner {
    pub(crate) fn new(profile_id: &str, session_id: &str, run_id: &str) -> Self {
        Self {
            profile_id: profile_id.to_owned(),
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
        }
    }

    fn matches_run(&self, profile_id: &str, run_id: &str) -> bool {
        self.profile_id == profile_id && self.run_id == run_id
    }
}

#[derive(Clone)]
pub(crate) struct BrowserManager {
    inner: Arc<BrowserManagerInner>,
}

struct BrowserManagerInner {
    root: PathBuf,
    executable: Option<PathBuf>,
    policy: EgressPolicy,
    sessions: Mutex<HashMap<BrowserOwner, Arc<Mutex<BrowserSession>>>>,
}

impl BrowserManager {
    pub(crate) fn new(root: &Path) -> Self {
        // Browser profiles are intentionally ephemeral. A process crash skips
        // TempDir cleanup, so conservatively remove only verified stale run
        // directories before accepting a new session.
        recover_browser_storage(root);
        Self {
            inner: Arc::new(BrowserManagerInner {
                root: root.to_owned(),
                executable: discover_browser_binary(),
                policy: EgressPolicy::production(),
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_binary(root: &Path, executable: PathBuf) -> Self {
        Self {
            inner: Arc::new(BrowserManagerInner {
                root: root.to_owned(),
                executable: Some(executable),
                policy: EgressPolicy::allow_loopback_for_tests(),
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Discovery alone is intentionally conservative: a missing binary keeps
    /// all Browser definitions out of a Run. Launch failures remain a normal
    /// per-Run unavailable error and never fall back to a shell/Node runtime.
    pub(crate) fn is_available(&self) -> bool {
        self.inner.executable.is_some()
    }

    /// A supported Chromium binary is the only runtime prerequisite once the
    /// isolated download implementation is compiled in. The tool registry
    /// still requires an owner-bound durable approval before it can enable a
    /// browser download for a Run.
    pub(crate) fn downloads_available(&self) -> bool {
        self.is_available()
    }

    pub(crate) async fn execute(
        &self,
        owner: BrowserOwner,
        action: BrowserAction,
        control: ToolExecutionControl,
        cancellation: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<BrowserToolOutput, BrowserError> {
        check_control(&control)?;
        let action_deadline = if matches!(&action, BrowserAction::Navigate { .. }) {
            deadline.min(Instant::now() + BROWSER_NAVIGATION_TIMEOUT)
        } else {
            deadline
        };
        if let BrowserAction::Navigate { url } = &action {
            self.inner.policy.ensure_url(url).await?;
        }
        if *cancellation.borrow() {
            return Err(BrowserError::Cancelled);
        }
        if Instant::now() >= action_deadline {
            return Err(BrowserError::DeadlineExceeded);
        }
        let session = self
            .session_for(
                owner.clone(),
                &control,
                cancellation.clone(),
                action_deadline,
            )
            .await?;
        let result = {
            let mut session = session.lock().await;
            session
                .execute(
                    &self.inner.policy,
                    action,
                    &control,
                    cancellation,
                    action_deadline,
                )
                .await
        };
        if matches!(result, Err(BrowserError::Crashed)) {
            self.cleanup_owner(&owner).await;
        }
        result
    }

    pub(crate) async fn cleanup_run(&self, profile_id: &str, run_id: &str) {
        let owners = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .keys()
                .filter(|owner| owner.matches_run(profile_id, run_id))
                .cloned()
                .collect::<Vec<_>>()
        };
        for owner in owners {
            self.cleanup_owner(&owner).await;
        }
    }

    pub(crate) async fn shutdown_all(&self) {
        let sessions = {
            let mut sessions = self.inner.sessions.lock().await;
            std::mem::take(&mut *sessions)
        };
        for session in sessions.into_values() {
            let mut session = session.lock().await;
            if let Err(error) = session.shutdown().await {
                tracing::warn!(?error, "failed to stop an isolated Browser session");
            }
        }
    }

    async fn cleanup_owner(&self, owner: &BrowserOwner) {
        let session = self.inner.sessions.lock().await.remove(owner);
        if let Some(session) = session {
            let mut session = session.lock().await;
            if let Err(error) = session.shutdown().await {
                tracing::warn!(?error, "failed to clean up an isolated Browser session");
            }
        }
    }

    async fn session_for(
        &self,
        owner: BrowserOwner,
        control: &ToolExecutionControl,
        cancellation: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Arc<Mutex<BrowserSession>>, BrowserError> {
        if let Some(session) = self.inner.sessions.lock().await.get(&owner).cloned() {
            return Ok(session);
        }
        let executable = self
            .inner
            .executable
            .as_ref()
            .ok_or(BrowserError::Unavailable)?
            .clone();
        // A concurrent tool for the same Run cannot exist in the Run engine,
        // but this second lookup keeps this type correct if that changes.
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(session) = sessions.get(&owner).cloned() {
            return Ok(session);
        }
        let session = BrowserSession::launch(
            executable,
            &self.inner.root,
            &owner,
            self.inner.policy.clone(),
            control,
            cancellation,
            deadline,
        )
        .await?;
        let session = Arc::new(Mutex::new(session));
        sessions.insert(owner, session.clone());
        Ok(session)
    }
}

pub(crate) fn browser_binary_available() -> bool {
    discover_browser_binary().is_some()
}

#[cfg(test)]
pub(crate) fn test_browser_binary() -> Option<PathBuf> {
    discover_browser_binary()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BrowserAction {
    Navigate {
        url: Url,
    },
    Snapshot,
    Click {
        selector: String,
        snapshot_id: String,
    },
    Type {
        selector: String,
        text: String,
        snapshot_id: String,
    },
    Scroll {
        delta_x: i64,
        delta_y: i64,
        snapshot_id: String,
    },
    Back {
        snapshot_id: String,
    },
    Press {
        key: String,
        snapshot_id: String,
    },
    GetImages,
    Vision {
        prompt: Option<String>,
    },
    Console {
        limit: usize,
    },
    Cdp {
        expression: String,
        snapshot_id: String,
    },
    Dialog {
        accept: bool,
        prompt_text: Option<String>,
        snapshot_id: String,
    },
    Download {
        selector: String,
        snapshot_id: String,
    },
}

impl BrowserAction {
    pub(crate) fn parse(tool_name: &str, raw_arguments_json: &str) -> Result<Self, BrowserError> {
        match tool_name {
            "browser_navigate" => {
                let input: NavigateInput = parse_input(raw_arguments_json)?;
                let url = parse_browser_url(&input.url)?;
                Ok(Self::Navigate { url })
            }
            "browser_snapshot" => {
                parse_empty_input(raw_arguments_json)?;
                Ok(Self::Snapshot)
            }
            "browser_click" => {
                let input: ClickInput = parse_input(raw_arguments_json)?;
                Ok(Self::Click {
                    selector: valid_selector(input.selector)?,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_type" => {
                let input: TypeInput = parse_input(raw_arguments_json)?;
                if input.text.is_empty() || input.text.chars().count() > 4_096 {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Type {
                    selector: valid_selector(input.selector)?,
                    text: input.text,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_scroll" => {
                let input: ScrollInput = parse_input(raw_arguments_json)?;
                if !(-4_000..=4_000).contains(&input.delta_y)
                    || !(-4_000..=4_000).contains(&input.delta_x)
                    || input.delta_x == 0 && input.delta_y == 0
                {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Scroll {
                    delta_x: input.delta_x,
                    delta_y: input.delta_y,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_back" => {
                let input: SnapshotInput = parse_input(raw_arguments_json)?;
                Ok(Self::Back {
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_press" => {
                let input: PressInput = parse_input(raw_arguments_json)?;
                if !valid_key(&input.key) {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Press {
                    key: input.key,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_get_images" => {
                parse_empty_input(raw_arguments_json)?;
                Ok(Self::GetImages)
            }
            "browser_vision" => {
                let input: VisionInput = parse_input(raw_arguments_json)?;
                if input
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.chars().count() > 1_000)
                {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Vision {
                    prompt: input.prompt,
                })
            }
            "browser_console" => {
                let input: ConsoleInput = parse_input(raw_arguments_json)?;
                if !(1..=50).contains(&input.limit) {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Console { limit: input.limit })
            }
            "browser_cdp" => {
                let input: CdpInput = parse_input(raw_arguments_json)?;
                if input.method != "Runtime.evaluate"
                    || input.expression.is_empty()
                    || input.expression.chars().count() > 8_192
                {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Cdp {
                    expression: input.expression,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_dialog" => {
                let input: DialogInput = parse_input(raw_arguments_json)?;
                if !matches!(input.action.as_str(), "accept" | "dismiss") {
                    return Err(BrowserError::InvalidArguments);
                }
                if input
                    .prompt_text
                    .as_ref()
                    .is_some_and(|text| text.chars().count() > 2_000)
                {
                    return Err(BrowserError::InvalidArguments);
                }
                Ok(Self::Dialog {
                    accept: input.action == "accept",
                    prompt_text: input.prompt_text,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            "browser_download" => {
                let input: ClickInput = parse_input(raw_arguments_json)?;
                Ok(Self::Download {
                    selector: valid_selector(input.selector)?,
                    snapshot_id: valid_snapshot_id(input.snapshot_id)?,
                })
            }
            _ => Err(BrowserError::InvalidArguments),
        }
    }

    pub(crate) fn input_summary(&self) -> String {
        match self {
            Self::Navigate { url } => {
                format!("Navigate browser to {}", url.origin().ascii_serialization())
            }
            Self::Snapshot => "Read browser accessibility snapshot".to_owned(),
            Self::Click { selector, .. } => {
                format!("Click browser element {}", bounded_text(selector, 120))
            }
            Self::Type { selector, .. } => {
                format!("Type into browser element {}", bounded_text(selector, 120))
            }
            Self::Scroll {
                delta_x, delta_y, ..
            } => format!("Scroll browser by {delta_x}, {delta_y}"),
            Self::Back { .. } => "Navigate browser back".to_owned(),
            Self::Press { key, .. } => format!("Press browser key {key}"),
            Self::GetImages => "Capture bounded browser screenshot".to_owned(),
            Self::Vision { .. } => "Capture bounded browser vision image".to_owned(),
            Self::Console { .. } => "Read browser console".to_owned(),
            Self::Cdp { .. } => "Run approved browser Runtime.evaluate".to_owned(),
            Self::Dialog { accept, .. } => if *accept {
                "Accept browser dialog"
            } else {
                "Dismiss browser dialog"
            }
            .to_owned(),
            Self::Download { selector, .. } => {
                format!(
                    "Download browser resource from {}",
                    bounded_text(selector, 120)
                )
            }
        }
    }

    pub(crate) fn approval_text(&self) -> Option<&str> {
        match self {
            Self::Type { text, .. } => Some(text),
            Self::Cdp { expression, .. } => Some(expression),
            Self::Dialog { prompt_text, .. } => prompt_text.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn requires_snapshot(&self) -> Option<&str> {
        match self {
            Self::Click { snapshot_id, .. }
            | Self::Type { snapshot_id, .. }
            | Self::Scroll { snapshot_id, .. }
            | Self::Back { snapshot_id }
            | Self::Press { snapshot_id, .. }
            | Self::Cdp { snapshot_id, .. }
            | Self::Dialog { snapshot_id, .. } => Some(snapshot_id),
            Self::Download { snapshot_id, .. } => Some(snapshot_id),
            Self::Navigate { .. }
            | Self::Snapshot
            | Self::GetImages
            | Self::Vision { .. }
            | Self::Console { .. } => None,
        }
    }
}

pub(crate) struct BrowserToolOutput {
    pub(crate) value: JsonValue,
    pub(crate) result_summary: String,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum BrowserError {
    #[error("browser is unavailable")]
    Unavailable,
    #[error("browser arguments are invalid")]
    InvalidArguments,
    #[error("browser target is blocked by the egress policy")]
    PolicyBlocked,
    #[error("browser snapshot precondition failed")]
    SnapshotRequired,
    #[error("browser operation was cancelled")]
    Cancelled,
    #[error("browser operation deadline elapsed")]
    DeadlineExceeded,
    #[error("browser process crashed")]
    Crashed,
    #[error("browser operation failed")]
    ExecutionFailed,
    #[error("browser download was rejected by the isolated download policy")]
    DownloadRejected,
}

#[derive(Clone)]
struct EgressPolicy {
    allow_loopback: bool,
}

impl EgressPolicy {
    fn production() -> Self {
        Self {
            allow_loopback: false,
        }
    }

    #[cfg(test)]
    fn allow_loopback_for_tests() -> Self {
        Self {
            allow_loopback: true,
        }
    }

    async fn ensure_url(&self, url: &Url) -> Result<Vec<SocketAddr>, BrowserError> {
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(BrowserError::PolicyBlocked);
        }
        let host = url.host_str().ok_or(BrowserError::PolicyBlocked)?;
        let port = url
            .port_or_known_default()
            .ok_or(BrowserError::PolicyBlocked)?;
        self.resolve(host, port).await
    }

    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, BrowserError> {
        if host.is_empty() || host.len() > 253 {
            return Err(BrowserError::PolicyBlocked);
        }
        let addresses = tokio::time::timeout(DNS_RESOLUTION_TIMEOUT, lookup_host((host, port)))
            .await
            .map_err(|_| BrowserError::PolicyBlocked)?
            .map_err(|_| BrowserError::PolicyBlocked)?
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|address| !self.permits_address(address.ip()))
        {
            return Err(BrowserError::PolicyBlocked);
        }
        Ok(addresses)
    }

    fn permits_address(&self, address: IpAddr) -> bool {
        if self.allow_loopback && address.is_loopback() {
            return true;
        }
        match address {
            IpAddr::V4(address) => public_ipv4(address),
            IpAddr::V6(address) => public_ipv6(address),
        }
    }
}

fn public_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || octets[0] == 0
        || octets[0] == 100 && (64..=127).contains(&octets[1])
        || octets[0] == 192 && octets[1] == 0 && octets[2] == 0
        || octets[0] == 192 && octets[1] == 0 && octets[2] == 2
        || octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
        || octets[0] == 198 && octets[1] == 51 && octets[2] == 100
        || octets[0] == 203 && octets[1] == 0 && octets[2] == 113
        || octets[0] >= 240)
}

fn public_ipv6(address: Ipv6Addr) -> bool {
    if address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_unicast_link_local()
    {
        return false;
    }
    let segments = address.segments();
    if (segments[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    if let Some(mapped) = address.to_ipv4_mapped() {
        return public_ipv4(mapped);
    }
    // Documentation and special-purpose allocation ranges must not become a
    // backdoor to local infrastructure through a resolver override.
    !(segments[0] == 0x2001 && segments[1] == 0x0db8)
}

struct EgressProxy {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    address: SocketAddr,
}

impl EgressProxy {
    async fn start(policy: EgressPolicy) -> Result<Self, BrowserError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| BrowserError::ExecutionFailed)?;
        let address = listener
            .local_addr()
            .map_err(|_| BrowserError::ExecutionFailed)?;
        let (shutdown, mut receiver) = watch::channel(false);
        let permits = Arc::new(Semaphore::new(PROXY_CONNECTION_LIMIT));
        let task = tokio::spawn(async move {
            let mut workers = JoinSet::new();
            loop {
                tokio::select! {
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        let policy = policy.clone();
                        let mut receiver = receiver.clone();
                        let permits = permits.clone();
                        workers.spawn(async move {
                            let Ok(permit) = permits.try_acquire_owned() else {
                                let mut stream = stream;
                                let _ = write_proxy_error(&mut stream, 503, "Proxy is busy").await;
                                return;
                            };
                            let _permit = permit;
                            let _ = tokio::select! {
                                _ = receiver.changed() => Ok(()),
                                result = proxy_connection(stream, policy) => result,
                            };
                        });
                    }
                    Some(_) = workers.join_next(), if !workers.is_empty() => {}
                }
            }
            workers.abort_all();
            while workers.join_next().await.is_some() {}
        });
        Ok(Self {
            shutdown,
            task: Some(task),
            address,
        })
    }

    fn address(&self) -> SocketAddr {
        self.address
    }

    async fn shutdown(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

async fn proxy_connection(mut client: TcpStream, policy: EgressPolicy) -> Result<(), BrowserError> {
    let (header, remainder) = read_proxy_header(&mut client).await?;
    let request = std::str::from_utf8(&header).map_err(|_| BrowserError::PolicyBlocked)?;
    let mut lines = request.split("\r\n");
    let first = lines.next().ok_or(BrowserError::PolicyBlocked)?;
    let mut parts = first.split_ascii_whitespace();
    let method = parts.next().ok_or(BrowserError::PolicyBlocked)?;
    let target = parts.next().ok_or(BrowserError::PolicyBlocked)?;
    let version = parts.next().ok_or(BrowserError::PolicyBlocked)?;
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        write_proxy_error(&mut client, 400, "Invalid proxy request").await?;
        return Ok(());
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = parse_connect_target(target)?;
        let addresses = policy.resolve(&host, port).await?;
        let mut upstream = connect_resolved(&addresses).await?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|_| BrowserError::ExecutionFailed)?;
        if !remainder.is_empty() {
            upstream
                .write_all(&remainder)
                .await
                .map_err(|_| BrowserError::ExecutionFailed)?;
        }
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .map_err(|_| BrowserError::ExecutionFailed)?;
        return Ok(());
    }
    let url = Url::parse(target).map_err(|_| BrowserError::PolicyBlocked)?;
    if url.scheme() != "http" {
        write_proxy_error(
            &mut client,
            400,
            "Only HTTP absolute-form requests are accepted",
        )
        .await?;
        return Ok(());
    }
    let addresses = policy.ensure_url(&url).await?;
    let rewritten = rewrite_http_header(request, method, version, &url)?;
    let mut upstream = connect_resolved(&addresses).await?;
    upstream
        .write_all(rewritten.as_bytes())
        .await
        .map_err(|_| BrowserError::ExecutionFailed)?;
    if !remainder.is_empty() {
        upstream
            .write_all(&remainder)
            .await
            .map_err(|_| BrowserError::ExecutionFailed)?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map_err(|_| BrowserError::ExecutionFailed)?;
    Ok(())
}

async fn read_proxy_header(client: &mut TcpStream) -> Result<(Vec<u8>, Vec<u8>), BrowserError> {
    let mut bytes = Vec::with_capacity(1_024);
    let mut buffer = [0u8; 2_048];
    loop {
        let read = tokio::time::timeout(Duration::from_secs(10), client.read(&mut buffer))
            .await
            .map_err(|_| BrowserError::ExecutionFailed)?
            .map_err(|_| BrowserError::ExecutionFailed)?;
        if read == 0 {
            return Err(BrowserError::PolicyBlocked);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > PROXY_HEADER_LIMIT {
            return Err(BrowserError::PolicyBlocked);
        }
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let end = end + 4;
            return Ok((bytes[..end].to_vec(), bytes[end..].to_vec()));
        }
    }
}

fn parse_connect_target(target: &str) -> Result<(String, u16), BrowserError> {
    let url = Url::parse(&format!("http://{target}/")).map_err(|_| BrowserError::PolicyBlocked)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BrowserError::PolicyBlocked);
    }
    let host = url.host_str().ok_or(BrowserError::PolicyBlocked)?;
    let port = url.port().ok_or(BrowserError::PolicyBlocked)?;
    Ok((host.to_owned(), port))
}

fn rewrite_http_header(
    request: &str,
    method: &str,
    version: &str,
    url: &Url,
) -> Result<String, BrowserError> {
    let mut output = String::new();
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    output.push_str(method);
    output.push(' ');
    output.push_str(path);
    if let Some(query) = url.query() {
        output.push('?');
        output.push_str(query);
    }
    output.push(' ');
    output.push_str(version);
    output.push_str("\r\n");
    let mut saw_host = false;
    for line in request.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        let (name, _) = line.split_once(':').ok_or(BrowserError::PolicyBlocked)?;
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
        {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            saw_host = true;
        }
        output.push_str(line);
        output.push_str("\r\n");
    }
    if !saw_host {
        output.push_str("Host: ");
        output.push_str(url.host_str().ok_or(BrowserError::PolicyBlocked)?);
        if let Some(port) = url.port() {
            output.push(':');
            output.push_str(&port.to_string());
        }
        output.push_str("\r\n");
    }
    output.push_str("\r\n");
    Ok(output)
}

async fn connect_resolved(addresses: &[SocketAddr]) -> Result<TcpStream, BrowserError> {
    for address in addresses {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address)).await
        {
            return Ok(stream);
        }
    }
    Err(BrowserError::ExecutionFailed)
}

async fn write_proxy_error(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
) -> Result<(), BrowserError> {
    let response =
        format!("HTTP/1.1 {status} {message}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|_| BrowserError::ExecutionFailed)
}

/// The browser is given the ambient path only long enough to let Chromium
/// write a download. All subsequent inspection is capability-relative to this
/// directory; the path never crosses the tool/API boundary.
struct DownloadStore {
    path: PathBuf,
    directory: Option<Dir>,
}

struct DownloadProjection {
    name: String,
    mime_type: &'static str,
    size_bytes: u64,
    sha256: String,
}

impl DownloadProjection {
    fn public_value(&self) -> JsonValue {
        json!({
            "name": self.name,
            "mimeType": self.mime_type,
            "sizeBytes": self.size_bytes,
            "sha256": self.sha256,
            "scan": {
                "status": "accepted",
                "checks": ["isolated_path", "filename", "mime", "size", "sha256"],
                "contentExposed": false,
                "workspaceImported": false,
            }
        })
    }
}

impl DownloadStore {
    fn create(profile_dir: &Path) -> Result<Self, BrowserError> {
        ensure_safe_directory(profile_dir)?;
        let parent = open_ambient_directory_nofollow(profile_dir)?;
        match parent.open_dir_nofollow(DOWNLOAD_DIRECTORY_NAME) {
            Ok(directory) => {
                drop(directory);
                return Err(BrowserError::ExecutionFailed);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(BrowserError::ExecutionFailed),
        }
        parent
            .create_dir(DOWNLOAD_DIRECTORY_NAME)
            .map_err(|_| BrowserError::ExecutionFailed)?;
        let directory = parent
            .open_dir_nofollow(DOWNLOAD_DIRECTORY_NAME)
            .map_err(|_| BrowserError::ExecutionFailed)?;
        let path = profile_dir.join(DOWNLOAD_DIRECTORY_NAME);
        ensure_safe_directory(&path)?;
        Ok(Self {
            path,
            directory: Some(directory),
        })
    }

    fn directory(&self) -> Result<&Dir, BrowserError> {
        self.directory.as_ref().ok_or(BrowserError::ExecutionFailed)
    }

    fn close(&mut self) {
        self.directory.take();
    }

    fn browser_path(&self) -> Result<&str, BrowserError> {
        self.path.to_str().ok_or(BrowserError::ExecutionFailed)
    }

    fn prepare_for_download(&self) -> Result<(), BrowserError> {
        self.cleanup_contents()
    }

    fn project_completed(
        &self,
        guid: &str,
        suggested_filename: &str,
        maximum_bytes: u64,
    ) -> Result<DownloadProjection, BrowserError> {
        if !valid_download_guid(guid) {
            return Err(BrowserError::DownloadRejected);
        }
        let name = valid_download_filename(suggested_filename)?;
        let entries = self.entry_names()?;
        if entries.len() != 1 || !entries.contains(guid) {
            return Err(BrowserError::DownloadRejected);
        }
        let metadata = self.safe_file_metadata(guid)?;
        if metadata.len() == 0
            || metadata.len() > maximum_bytes
            || metadata.len() > MAX_DOWNLOAD_BYTES
        {
            return Err(BrowserError::DownloadRejected);
        }
        let mut options = CapOpenOptions::new();
        options.read(true);
        options.follow(FollowSymlinks::No);
        let mut file = self
            .directory()?
            .open_with(guid, &options)
            .map_err(|_| BrowserError::DownloadRejected)?;
        let opened = file
            .metadata()
            .map_err(|_| BrowserError::DownloadRejected)?;
        if !opened.is_file() || opened.len() != metadata.len() || opened.len() > maximum_bytes {
            return Err(BrowserError::DownloadRejected);
        }
        let mut hash = Sha256::new();
        let mut scan_bytes = Vec::with_capacity(MAX_DOWNLOAD_SCAN_BYTES);
        let mut total = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| BrowserError::DownloadRejected)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(read as u64)
                .ok_or(BrowserError::DownloadRejected)?;
            if total > maximum_bytes || total > MAX_DOWNLOAD_BYTES {
                return Err(BrowserError::DownloadRejected);
            }
            hash.update(&buffer[..read]);
            let remaining = MAX_DOWNLOAD_SCAN_BYTES.saturating_sub(scan_bytes.len());
            scan_bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        if total == 0 || total != metadata.len() {
            return Err(BrowserError::DownloadRejected);
        }
        let mime_type = detect_download_mime(&name, &scan_bytes)?;
        Ok(DownloadProjection {
            name,
            mime_type,
            size_bytes: total,
            sha256: format!("{:x}", hash.finalize()),
        })
    }

    fn cleanup_contents(&self) -> Result<(), BrowserError> {
        for name in self.entry_names()? {
            let metadata = self
                .directory()?
                .symlink_metadata(&name)
                .map_err(|_| BrowserError::ExecutionFailed)?;
            if metadata.file_type().is_symlink() || is_windows_reparse(&self.path.join(&name)) {
                return Err(BrowserError::DownloadRejected);
            }
            if metadata.is_file() {
                self.directory()?
                    .remove_file(&name)
                    .map_err(|_| BrowserError::ExecutionFailed)?;
            } else {
                // A download directory is flat by contract. Do not recurse
                // into an unexpected directory, device, or reparse target.
                return Err(BrowserError::DownloadRejected);
            }
        }
        Ok(())
    }

    fn entry_names(&self) -> Result<BTreeSet<String>, BrowserError> {
        let mut entries = BTreeSet::new();
        for entry in self
            .directory()?
            .entries()
            .map_err(|_| BrowserError::ExecutionFailed)?
        {
            let entry = entry.map_err(|_| BrowserError::ExecutionFailed)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(BrowserError::DownloadRejected);
            };
            if !valid_download_entry_name(name) || !entries.insert(name.to_owned()) {
                return Err(BrowserError::DownloadRejected);
            }
            if entries.len() > MAX_DOWNLOAD_DIRECTORY_ENTRIES {
                return Err(BrowserError::DownloadRejected);
            }
        }
        Ok(entries)
    }

    fn safe_file_metadata(&self, name: &str) -> Result<fs::Metadata, BrowserError> {
        let metadata = self
            .directory()?
            .symlink_metadata(name)
            .map_err(|_| BrowserError::DownloadRejected)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || is_windows_reparse(&self.path.join(name))
        {
            return Err(BrowserError::DownloadRejected);
        }
        // Re-read from the OS path only to inspect Windows' reparse bit. The
        // following capability-relative no-follow open is the authority check
        // that closes the time-of-check/use window.
        let native = fs::symlink_metadata(self.path.join(name))
            .map_err(|_| BrowserError::DownloadRejected)?;
        if native.file_type().is_symlink()
            || !native.is_file()
            || is_windows_reparse(&self.path.join(name))
        {
            return Err(BrowserError::DownloadRejected);
        }
        Ok(native)
    }
}

fn valid_download_guid(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_download_entry_name(value: &str) -> bool {
    valid_download_guid(value)
        || value
            .strip_suffix(".crdownload")
            .is_some_and(valid_download_guid)
}

fn valid_download_filename(value: &str) -> Result<String, BrowserError> {
    if value.is_empty()
        || value.chars().count() > MAX_DOWNLOAD_FILENAME_CHARS
        || value.starts_with('.')
        || value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | ':' | '\0') || character.is_control())
    {
        return Err(BrowserError::DownloadRejected);
    }
    let path = Path::new(value);
    if path.components().count() != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(value)
    {
        return Err(BrowserError::DownloadRejected);
    }
    Ok(value.to_owned())
}

fn detect_download_mime(name: &str, bytes: &[u8]) -> Result<&'static str, BrowserError> {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .ok_or(BrowserError::DownloadRejected)?;
    let magic_mime = if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        Some("application/zip")
    } else {
        None
    };
    match extension.as_str() {
        "pdf" if magic_mime == Some("application/pdf") => Ok("application/pdf"),
        "png" if magic_mime == Some("image/png") => Ok("image/png"),
        "jpg" | "jpeg" if magic_mime == Some("image/jpeg") => Ok("image/jpeg"),
        "gif" if magic_mime == Some("image/gif") => Ok("image/gif"),
        "webp" if magic_mime == Some("image/webp") => Ok("image/webp"),
        "zip" if magic_mime == Some("application/zip") => Ok("application/zip"),
        "docx" if magic_mime == Some("application/zip") => {
            Ok("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        }
        "xlsx" if magic_mime == Some("application/zip") => {
            Ok("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        }
        "txt" | "md" | "csv" | "tsv" | "json" | "yaml" | "yml" => {
            let text = std::str::from_utf8(bytes).map_err(|_| BrowserError::DownloadRejected)?;
            if text
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
            {
                return Err(BrowserError::DownloadRejected);
            }
            match extension.as_str() {
                "json" if serde_json::from_str::<JsonValue>(text).is_ok() => Ok("application/json"),
                "json" => Err(BrowserError::DownloadRejected),
                "csv" => Ok("text/csv"),
                "tsv" => Ok("text/tab-separated-values"),
                "yaml" | "yml" => Ok("text/yaml"),
                "md" => Ok("text/markdown"),
                _ => Ok("text/plain"),
            }
        }
        _ => Err(BrowserError::DownloadRejected),
    }
}

fn ensure_safe_directory(path: &Path) -> Result<(), BrowserError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| BrowserError::ExecutionFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || is_windows_reparse(path) {
        Err(BrowserError::ExecutionFailed)
    } else {
        Ok(())
    }
}

fn open_ambient_directory_nofollow(path: &Path) -> Result<Dir, BrowserError> {
    let parent_path = path.parent().ok_or(BrowserError::ExecutionFailed)?;
    let name = path.file_name().ok_or(BrowserError::ExecutionFailed)?;
    let canonical_parent =
        fs::canonicalize(parent_path).map_err(|_| BrowserError::ExecutionFailed)?;
    let parent = Dir::open_ambient_dir(canonical_parent, ambient_authority())
        .map_err(|_| BrowserError::ExecutionFailed)?;
    parent
        .open_dir_nofollow(name)
        .map_err(|_| BrowserError::ExecutionFailed)
}

#[cfg(windows)]
fn is_windows_reparse(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fs::symlink_metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(not(windows))]
fn is_windows_reparse(_path: &Path) -> bool {
    false
}

fn browser_profile_root(root: &Path, owner: &BrowserOwner) -> Result<PathBuf, BrowserError> {
    ensure_safe_directory(root)?;
    let mut current = root.to_owned();
    for component in [".synthchat", "browser", "profiles"] {
        current = create_safe_child_directory(&current, component)?;
    }
    create_safe_child_directory(
        &current,
        &format!("profile-{}", owner_profile_digest(owner)),
    )
}

fn create_safe_child_directory(parent: &Path, name: &str) -> Result<PathBuf, BrowserError> {
    ensure_safe_directory(parent)?;
    let child = parent.join(name);
    match fs::symlink_metadata(&child) {
        Ok(_) => ensure_safe_directory(&child)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&child).map_err(|_| BrowserError::ExecutionFailed)?;
            ensure_safe_directory(&child)?;
        }
        Err(_) => return Err(BrowserError::ExecutionFailed),
    }
    Ok(child)
}

fn owner_profile_digest(owner: &BrowserOwner) -> String {
    let mut hash = Sha256::new();
    hash.update(owner.profile_id.as_bytes());
    let digest = format!("{:x}", hash.finalize());
    digest[..24].to_owned()
}

fn owner_run_prefix(owner: &BrowserOwner) -> String {
    let mut hash = Sha256::new();
    hash.update(owner.profile_id.as_bytes());
    hash.update([0]);
    hash.update(owner.session_id.as_bytes());
    hash.update([0]);
    hash.update(owner.run_id.as_bytes());
    let digest = format!("{:x}", hash.finalize());
    format!("run-{}-", &digest[..24])
}

fn recover_browser_storage(root: &Path) {
    let profiles = root.join(".synthchat").join("browser").join("profiles");
    let Ok(metadata) = fs::symlink_metadata(&profiles) else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() || is_windows_reparse(&profiles) {
        return;
    }
    let Ok(profile_entries) = fs::read_dir(&profiles) else {
        return;
    };
    for profile in profile_entries.flatten() {
        let profile_path = profile.path();
        let profile_name = profile.file_name();
        let Some(profile_name) = profile_name.to_str() else {
            continue;
        };
        let Ok(metadata) = fs::symlink_metadata(&profile_path) else {
            continue;
        };
        if !profile_name.starts_with("profile-")
            || metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || is_windows_reparse(&profile_path)
        {
            continue;
        }
        let Ok(run_entries) = fs::read_dir(&profile_path) else {
            continue;
        };
        for run in run_entries.flatten() {
            let run_path = run.path();
            let run_name = run.file_name();
            let Some(run_name) = run_name.to_str() else {
                continue;
            };
            let Ok(metadata) = fs::symlink_metadata(&run_path) else {
                continue;
            };
            if !run_name.starts_with("run-")
                || metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || is_windows_reparse(&run_path)
            {
                continue;
            }
            let mut entries = 0;
            if verify_recovery_tree(&run_path, 0, &mut entries).is_ok() {
                let canonical_parent = fs::canonicalize(&profile_path);
                let canonical_run = fs::canonicalize(&run_path);
                if canonical_parent
                    .as_ref()
                    .ok()
                    .zip(canonical_run.as_ref().ok())
                    .is_some_and(|(parent, run)| run.parent() == Some(parent.as_path()))
                {
                    let _ = fs::remove_dir_all(run_path);
                }
            }
        }
    }
}

fn verify_recovery_tree(
    directory: &Path,
    depth: usize,
    entries: &mut usize,
) -> Result<(), BrowserError> {
    if depth > MAX_RECOVERY_TREE_DEPTH || is_windows_reparse(directory) {
        return Err(BrowserError::ExecutionFailed);
    }
    for entry in fs::read_dir(directory).map_err(|_| BrowserError::ExecutionFailed)? {
        let entry = entry.map_err(|_| BrowserError::ExecutionFailed)?;
        *entries = entries
            .checked_add(1)
            .ok_or(BrowserError::ExecutionFailed)?;
        if *entries > MAX_RECOVERY_TREE_ENTRIES {
            return Err(BrowserError::ExecutionFailed);
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|_| BrowserError::ExecutionFailed)?;
        if metadata.file_type().is_symlink() || is_windows_reparse(&path) {
            return Err(BrowserError::ExecutionFailed);
        }
        if metadata.is_dir() {
            verify_recovery_tree(&path, depth + 1, entries)?;
        } else if !metadata.is_file() {
            return Err(BrowserError::ExecutionFailed);
        }
    }
    Ok(())
}

struct BrowserSession {
    child: Child,
    pid: u32,
    lifetime: Option<platform::ProcessLifetime>,
    profile_dir: Option<TempDir>,
    proxy: EgressProxy,
    cdp: CdpClient,
    target_id: String,
    session_id: String,
    snapshot: Option<SnapshotPrecondition>,
    downloads: DownloadStore,
    downloaded_bytes: u64,
    downloaded_count: usize,
}

impl BrowserSession {
    async fn launch(
        executable: PathBuf,
        root: &Path,
        owner: &BrowserOwner,
        policy: EgressPolicy,
        control: &ToolExecutionControl,
        mut cancellation: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Self, BrowserError> {
        check_control(control)?;
        if *cancellation.borrow() {
            return Err(BrowserError::Cancelled);
        }
        let proxy = EgressProxy::start(policy).await?;
        let profiles_root = browser_profile_root(root, owner)?;
        let run_prefix = owner_run_prefix(owner);
        let profile_dir = TempBuilder::new()
            .prefix(&run_prefix)
            .tempdir_in(&profiles_root)
            .map_err(|_| BrowserError::ExecutionFailed)?;
        let downloads = DownloadStore::create(profile_dir.path())?;
        let mut command = Command::new(executable);
        platform::configure_spawn(&mut command, true).map_err(|_| BrowserError::ExecutionFailed)?;
        command
            .arg("--headless=new")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg("--disable-background-networking")
            .arg("--disable-component-update")
            .arg("--disable-client-side-phishing-detection")
            .arg("--disable-sync")
            .arg("--disable-quic")
            .arg("--disable-popup-blocking")
            .arg("--disable-features=DownloadBubble,DownloadLater")
            .arg("--disable-gpu")
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=http://127.0.0.1")
            .arg("--proxy-bypass-list=<-loopback>")
            .arg(format!("--proxy-server=http://{}", proxy.address()))
            .arg(format!("--user-data-dir={}", profile_dir.path().display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = command.spawn().map_err(|_| BrowserError::Unavailable)?;
        let Some(pid) = child.id() else {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            let mut proxy = proxy;
            proxy.shutdown().await;
            return Err(BrowserError::ExecutionFailed);
        };
        let lifetime = match platform::process_lifetime(pid) {
            Ok(lifetime) => lifetime,
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                let mut proxy = proxy;
                proxy.shutdown().await;
                return Err(BrowserError::ExecutionFailed);
            }
        };
        let launch_deadline = (Instant::now() + BROWSER_START_TIMEOUT).min(deadline);
        let setup = async {
            let port = wait_for_devtools_port(
                &profile_dir,
                &mut child,
                control,
                &mut cancellation,
                launch_deadline,
            )
            .await?;
            let endpoint = cdp_endpoint(port).await?;
            let mut cdp = CdpClient::connect(&endpoint, launch_deadline).await?;
            let (_never_cancel_sender, mut never_cancel) = watch::channel(false);
            cdp.call(
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny", "eventsEnabled": false}),
                None,
                None,
                &mut never_cancel,
                launch_deadline,
            )
            .await?;
            let target = cdp
                .call(
                    "Target.createTarget",
                    json!({"url": "about:blank"}),
                    None,
                    None,
                    &mut never_cancel,
                    launch_deadline,
                )
                .await?;
            let target_id = target
                .get("targetId")
                .and_then(JsonValue::as_str)
                .ok_or(BrowserError::ExecutionFailed)?
                .to_owned();
            let attached = cdp
                .call(
                    "Target.attachToTarget",
                    json!({"targetId": target_id, "flatten": true}),
                    None,
                    None,
                    &mut never_cancel,
                    launch_deadline,
                )
                .await?;
            let session_id = attached
                .get("sessionId")
                .and_then(JsonValue::as_str)
                .ok_or(BrowserError::ExecutionFailed)?
                .to_owned();
            cdp.call(
                "Page.enable",
                json!({}),
                Some(&session_id),
                None,
                &mut never_cancel,
                launch_deadline,
            )
            .await?;
            cdp.call(
                "Runtime.enable",
                json!({}),
                Some(&session_id),
                None,
                &mut never_cancel,
                launch_deadline,
            )
            .await?;
            Ok::<_, BrowserError>((cdp, target_id, session_id))
        }
        .await;
        let (cdp, target_id, session_id) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                let _ = platform::finish_after_root_exit(pid, lifetime);
                let mut proxy = proxy;
                proxy.shutdown().await;
                return Err(error);
            }
        };
        Ok(Self {
            child,
            pid,
            lifetime: Some(lifetime),
            profile_dir: Some(profile_dir),
            proxy,
            cdp,
            target_id,
            session_id,
            snapshot: None,
            downloads,
            downloaded_bytes: 0,
            downloaded_count: 0,
        })
    }

    async fn execute(
        &mut self,
        policy: &EgressPolicy,
        action: BrowserAction,
        control: &ToolExecutionControl,
        mut cancellation: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<BrowserToolOutput, BrowserError> {
        check_control(control)?;
        if *cancellation.borrow() {
            return Err(BrowserError::Cancelled);
        }
        if self
            .child
            .try_wait()
            .map_err(|_| BrowserError::Crashed)?
            .is_some()
        {
            return Err(BrowserError::Crashed);
        }
        if let Some(snapshot_id) = action.requires_snapshot() {
            self.require_snapshot(snapshot_id)?;
        }
        match action {
            BrowserAction::Navigate { url } => {
                policy.ensure_url(&url).await?;
                let navigation_deadline = deadline.min(Instant::now() + BROWSER_NAVIGATION_TIMEOUT);
                let response = self
                    .call(
                        "Page.navigate",
                        json!({"url": url.as_str()}),
                        control,
                        &mut cancellation,
                        navigation_deadline,
                    )
                    .await?;
                if response
                    .get("errorText")
                    .and_then(JsonValue::as_str)
                    .is_some()
                {
                    return Err(BrowserError::ExecutionFailed);
                }
                self.wait_for_load(control, &mut cancellation, navigation_deadline)
                    .await?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"url": url.as_str(), "navigated": true}),
                    result_summary: "Browser navigation completed".to_owned(),
                })
            }
            BrowserAction::Snapshot => self.snapshot(control, &mut cancellation, deadline).await,
            BrowserAction::Click { selector, .. } => {
                self.click_selector(&selector, control, &mut cancellation, deadline)
                    .await?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"clicked": true, "selector": selector}),
                    result_summary: "Browser element clicked".to_owned(),
                })
            }
            BrowserAction::Type { selector, text, .. } => {
                self.focus_element(&selector, control, &mut cancellation, deadline)
                    .await?;
                self.call(
                    "Input.insertText",
                    json!({"text": text}),
                    control,
                    &mut cancellation,
                    deadline,
                )
                .await?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"typed": true, "selector": selector, "characters": text.chars().count()}),
                    result_summary: "Browser text entered".to_owned(),
                })
            }
            BrowserAction::Scroll {
                delta_x, delta_y, ..
            } => {
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseWheel", "x": 1, "y": 1, "deltaX": delta_x, "deltaY": delta_y}),
                    control,
                    &mut cancellation,
                    deadline,
                )
                .await?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"scrolled": true, "deltaX": delta_x, "deltaY": delta_y}),
                    result_summary: "Browser scrolled".to_owned(),
                })
            }
            BrowserAction::Back { .. } => {
                let history = self
                    .call(
                        "Page.getNavigationHistory",
                        json!({}),
                        control,
                        &mut cancellation,
                        deadline,
                    )
                    .await?;
                let index = history
                    .get("currentIndex")
                    .and_then(JsonValue::as_u64)
                    .ok_or(BrowserError::ExecutionFailed)?;
                if index == 0 {
                    return Ok(BrowserToolOutput {
                        value: json!({"navigated": false, "reason": "no_back_entry"}),
                        result_summary: "Browser has no back entry".to_owned(),
                    });
                }
                let entry_id = history
                    .get("entries")
                    .and_then(JsonValue::as_array)
                    .and_then(|entries| entries.get(index.saturating_sub(1) as usize))
                    .and_then(|entry| entry.get("id"))
                    .and_then(JsonValue::as_i64)
                    .ok_or(BrowserError::ExecutionFailed)?;
                self.call(
                    "Page.navigateToHistoryEntry",
                    json!({"entryId": entry_id}),
                    control,
                    &mut cancellation,
                    deadline,
                )
                .await?;
                self.wait_for_load(control, &mut cancellation, deadline)
                    .await?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"navigated": true}),
                    result_summary: "Browser navigated back".to_owned(),
                })
            }
            BrowserAction::Press { key, .. } => {
                self.call(
                    "Input.dispatchKeyEvent",
                    json!({"type": "keyDown", "key": key}),
                    control,
                    &mut cancellation,
                    deadline,
                )
                .await?;
                self.call(
                    "Input.dispatchKeyEvent",
                    json!({"type": "keyUp", "key": key}),
                    control,
                    &mut cancellation,
                    deadline,
                )
                .await?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"pressed": true, "key": key}),
                    result_summary: "Browser key pressed".to_owned(),
                })
            }
            BrowserAction::GetImages => {
                self.screenshot(None, control, &mut cancellation, deadline)
                    .await
            }
            BrowserAction::Vision { prompt } => {
                self.screenshot(prompt, control, &mut cancellation, deadline)
                    .await
            }
            BrowserAction::Console { limit } => {
                let entries = self.cdp.console_entries(limit);
                Ok(BrowserToolOutput {
                    value: json!({"entries": entries}),
                    result_summary: format!("{} browser console entries returned", entries.len()),
                })
            }
            BrowserAction::Cdp { expression, .. } => {
                let result = self
                    .call(
                        "Runtime.evaluate",
                        json!({
                            "expression": expression,
                            "returnByValue": true,
                            "awaitPromise": false,
                            "userGesture": false,
                            "includeCommandLineAPI": false,
                        }),
                        control,
                        &mut cancellation,
                        deadline,
                    )
                    .await?;
                let bounded = bounded_json_value(result, MAX_SNAPSHOT_BYTES)?;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"result": bounded}),
                    result_summary: "Approved CDP Runtime.evaluate completed".to_owned(),
                })
            }
            BrowserAction::Dialog {
                accept,
                prompt_text,
                ..
            } => {
                if self.cdp.dialog.is_none() {
                    return Err(BrowserError::ExecutionFailed);
                }
                self.call(
                    "Page.handleJavaScriptDialog",
                    json!({"accept": accept, "promptText": prompt_text}),
                    control,
                    &mut cancellation,
                    deadline,
                )
                .await?;
                self.cdp.dialog = None;
                self.snapshot = None;
                Ok(BrowserToolOutput {
                    value: json!({"handled": true, "action": if accept { "accept" } else { "dismiss" }}),
                    result_summary: "Browser dialog handled".to_owned(),
                })
            }
            BrowserAction::Download { selector, .. } => {
                self.download_from_selector(&selector, control, &mut cancellation, deadline)
                    .await
            }
        }
    }

    async fn click_selector(
        &mut self,
        selector: &str,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(), BrowserError> {
        let point = self
            .element_center(selector, control, cancellation, deadline)
            .await?;
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mousePressed", "x": point.0, "y": point.1, "button": "left", "clickCount": 1}),
            control,
            cancellation,
            deadline,
        )
        .await?;
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseReleased", "x": point.0, "y": point.1, "button": "left", "clickCount": 1}),
            control,
            cancellation,
            deadline,
        )
        .await
        .map(|_| ())
    }

    async fn download_from_selector(
        &mut self,
        selector: &str,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<BrowserToolOutput, BrowserError> {
        if self.downloaded_count >= MAX_RUN_DOWNLOADS
            || self.downloaded_bytes >= MAX_RUN_DOWNLOAD_BYTES
        {
            return Err(BrowserError::DownloadRejected);
        }
        self.downloads.prepare_for_download()?;
        self.cdp.clear_downloads();
        self.enable_downloads(control, cancellation, deadline)
            .await?;
        let maximum_bytes =
            MAX_DOWNLOAD_BYTES.min(MAX_RUN_DOWNLOAD_BYTES.saturating_sub(self.downloaded_bytes));
        let result = async {
            self.click_selector(selector, control, cancellation, deadline)
                .await?;
            self.snapshot = None;
            let observed = self
                .cdp
                .wait_for_download(
                    maximum_bytes,
                    Some(control),
                    cancellation,
                    deadline.min(Instant::now() + MAX_DOWNLOAD_WAIT),
                )
                .await?;
            let projection = self.downloads.project_completed(
                &observed.guid,
                &observed.suggested_filename,
                maximum_bytes,
            )?;
            self.downloaded_bytes = self
                .downloaded_bytes
                .checked_add(projection.size_bytes)
                .ok_or(BrowserError::DownloadRejected)?;
            self.downloaded_count = self
                .downloaded_count
                .checked_add(1)
                .ok_or(BrowserError::DownloadRejected)?;
            Ok::<_, BrowserError>(BrowserToolOutput {
                value: json!({"download": projection.public_value()}),
                result_summary: format!(
                    "Browser download accepted after isolated safety scan ({} bytes)",
                    projection.size_bytes
                ),
            })
        }
        .await;

        // This uses a fresh uncancelled receiver on purpose. A Run cancel or
        // deadline must not leave Chromium in an allow-download state.
        let disable_result = self.disable_downloads().await;
        let cleanup_result = self.downloads.cleanup_contents();
        match result {
            Ok(output) => {
                disable_result?;
                cleanup_result?;
                Ok(output)
            }
            Err(error) => {
                let _ = disable_result;
                let _ = cleanup_result;
                Err(error)
            }
        }
    }

    async fn enable_downloads(
        &mut self,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(), BrowserError> {
        let download_path = self.downloads.browser_path()?.to_owned();
        self.cdp
            .call(
                "Browser.setDownloadBehavior",
                json!({
                    "behavior": "allowAndName",
                    "downloadPath": download_path,
                    "eventsEnabled": true,
                }),
                None,
                Some(control),
                cancellation,
                deadline.min(Instant::now() + CDP_COMMAND_TIMEOUT),
            )
            .await?;
        Ok(())
    }

    async fn disable_downloads(&mut self) -> Result<(), BrowserError> {
        let (_sender, mut cancellation) = watch::channel(false);
        self.cdp
            .call(
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny", "eventsEnabled": false}),
                None,
                None,
                &mut cancellation,
                Instant::now() + Duration::from_secs(2),
            )
            .await?;
        Ok(())
    }

    async fn call(
        &mut self,
        method: &str,
        params: JsonValue,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<JsonValue, BrowserError> {
        self.cdp
            .call(
                method,
                params,
                Some(&self.session_id),
                Some(control),
                cancellation,
                deadline.min(Instant::now() + CDP_COMMAND_TIMEOUT),
            )
            .await
    }

    fn require_snapshot(&self, snapshot_id: &str) -> Result<(), BrowserError> {
        if self.snapshot.as_ref().map(|snapshot| snapshot.id.as_str()) == Some(snapshot_id) {
            Ok(())
        } else {
            Err(BrowserError::SnapshotRequired)
        }
    }

    async fn snapshot(
        &mut self,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<BrowserToolOutput, BrowserError> {
        let tree = self
            .call(
                "Accessibility.getFullAXTree",
                json!({}),
                control,
                cancellation,
                deadline,
            )
            .await?;
        let nodes = project_ax_nodes(&tree)?;
        let history = self
            .call(
                "Page.getNavigationHistory",
                json!({}),
                control,
                cancellation,
                deadline,
            )
            .await?;
        let url = current_history_url(&history).unwrap_or_else(|| "about:blank".to_owned());
        let title = self
            .call(
                "Runtime.evaluate",
                json!({"expression": "document.title", "returnByValue": true, "awaitPromise": false}),
                control,
                cancellation,
                deadline,
            )
            .await?
            .pointer("/result/value")
            .and_then(JsonValue::as_str)
            .map(|value| bounded_text(value, 500));
        let snapshot_id = format!("snapshot_{}", Uuid::new_v4().simple());
        let value = bounded_snapshot_value(&snapshot_id, &url, title.as_deref(), nodes)?;
        self.snapshot = Some(SnapshotPrecondition {
            id: snapshot_id.clone(),
        });
        Ok(BrowserToolOutput {
            value,
            result_summary: format!("Browser accessibility snapshot {snapshot_id} captured"),
        })
    }

    async fn element_center(
        &mut self,
        selector: &str,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(f64, f64), BrowserError> {
        let expression = format!(
            "(() => {{ const el = document.querySelector({}); if (!el) return null; el.scrollIntoView({{block: 'center', inline: 'center'}}); const r = el.getBoundingClientRect(); return {{x: r.left + r.width / 2, y: r.top + r.height / 2, width: r.width, height: r.height}}; }})()",
            serde_json::to_string(selector).map_err(|_| BrowserError::InvalidArguments)?
        );
        let result = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": false}),
                control,
                cancellation,
                deadline,
            )
            .await?;
        let value = result
            .pointer("/result/value")
            .and_then(JsonValue::as_object)
            .ok_or(BrowserError::ExecutionFailed)?;
        let x = value
            .get("x")
            .and_then(JsonValue::as_f64)
            .ok_or(BrowserError::ExecutionFailed)?;
        let y = value
            .get("y")
            .and_then(JsonValue::as_f64)
            .ok_or(BrowserError::ExecutionFailed)?;
        let width = value
            .get("width")
            .and_then(JsonValue::as_f64)
            .ok_or(BrowserError::ExecutionFailed)?;
        let height = value
            .get("height")
            .and_then(JsonValue::as_f64)
            .ok_or(BrowserError::ExecutionFailed)?;
        if !x.is_finite() || !y.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(BrowserError::ExecutionFailed);
        }
        Ok((x, y))
    }

    async fn focus_element(
        &mut self,
        selector: &str,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(), BrowserError> {
        let expression = format!(
            "(() => {{ const el = document.querySelector({}); if (!el) return false; el.scrollIntoView({{block: 'center', inline: 'center'}}); el.focus(); return document.activeElement === el; }})()",
            serde_json::to_string(selector).map_err(|_| BrowserError::InvalidArguments)?
        );
        let result = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": false}),
                control,
                cancellation,
                deadline,
            )
            .await?;
        if result.pointer("/result/value").and_then(JsonValue::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(BrowserError::ExecutionFailed)
        }
    }

    async fn wait_for_load(
        &mut self,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(), BrowserError> {
        // `Page.loadEventFired` can race the `Page.navigate` response on a
        // fast local page. Polling the page's own readyState avoids treating a
        // valid early event as a timeout while still observing cancellation.
        loop {
            let state = self
                .call(
                    "Runtime.evaluate",
                    json!({"expression": "document.readyState", "returnByValue": true, "awaitPromise": false}),
                    control,
                    cancellation,
                    deadline,
                )
                .await?
                .pointer("/result/value")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_owned();
            if navigation_document_ready(&state) {
                return Ok(());
            }
            tokio::select! {
                _ = cancellation.changed() => return Err(BrowserError::Cancelled),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    async fn screenshot(
        &mut self,
        prompt: Option<String>,
        control: &ToolExecutionControl,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<BrowserToolOutput, BrowserError> {
        let metrics = self
            .call(
                "Page.getLayoutMetrics",
                json!({}),
                control,
                cancellation,
                deadline,
            )
            .await?;
        let viewport = metrics
            .get("visualViewport")
            .and_then(JsonValue::as_object)
            .ok_or(BrowserError::ExecutionFailed)?;
        let width = viewport
            .get("clientWidth")
            .and_then(JsonValue::as_f64)
            .unwrap_or(MAX_SCREENSHOT_WIDTH)
            .clamp(1.0, MAX_SCREENSHOT_WIDTH);
        let height = viewport
            .get("clientHeight")
            .and_then(JsonValue::as_f64)
            .unwrap_or(MAX_SCREENSHOT_HEIGHT)
            .clamp(1.0, MAX_SCREENSHOT_HEIGHT);
        let captured = self
            .call(
                "Page.captureScreenshot",
                json!({
                    "format": "jpeg",
                    "quality": 45,
                    "optimizeForSpeed": true,
                    "captureBeyondViewport": false,
                    "clip": {"x": 0, "y": 0, "width": width, "height": height, "scale": 1},
                }),
                control,
                cancellation,
                deadline,
            )
            .await?;
        let data = captured
            .get("data")
            .and_then(JsonValue::as_str)
            .ok_or(BrowserError::ExecutionFailed)?;
        if data.len() > MAX_IMAGE_BASE64_CHARS {
            return Err(BrowserError::ExecutionFailed);
        }
        let mut value = json!({
            "format": "jpeg",
            "dataUrl": format!("data:image/jpeg;base64,{data}"),
            "width": width,
            "height": height,
        });
        if let Some(prompt) = prompt {
            value["prompt"] = JsonValue::String(prompt);
        }
        if serde_json::to_vec(&value)
            .map_err(|_| BrowserError::ExecutionFailed)?
            .len()
            > MAX_SNAPSHOT_BYTES
        {
            return Err(BrowserError::ExecutionFailed);
        }
        Ok(BrowserToolOutput {
            value,
            result_summary: "Bounded browser screenshot captured".to_owned(),
        })
    }

    async fn shutdown(&mut self) -> Result<(), BrowserError> {
        let (termination, completion) = if let Some(lifetime) = self.lifetime.take() {
            let _ = self.cdp.close(&self.target_id).await;
            let termination = platform::terminate_tree(self.pid, &mut self.child, &lifetime)
                .await
                .map_err(|_| BrowserError::ExecutionFailed);
            let completion = platform::finish_after_root_exit(self.pid, lifetime)
                .map_err(|_| BrowserError::ExecutionFailed);
            (termination, completion)
        } else {
            (Ok(()), Ok(()))
        };
        self.proxy.shutdown().await;
        let profile_cleanup = self.cleanup_profile().await;
        termination?;
        completion?;
        profile_cleanup
    }

    async fn cleanup_profile(&mut self) -> Result<(), BrowserError> {
        self.downloads.close();
        let Some(profile_dir) = self.profile_dir.as_ref() else {
            return Ok(());
        };
        let path = profile_dir.path().to_owned();
        let mut last_error = None;
        for attempt in 0..BROWSER_PROFILE_CLEANUP_ATTEMPTS {
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => {
                    self.profile_dir.take();
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.profile_dir.take();
                    return Ok(());
                }
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < BROWSER_PROFILE_CLEANUP_ATTEMPTS {
                tokio::time::sleep(BROWSER_PROFILE_CLEANUP_INTERVAL).await;
            }
        }
        tracing::warn!(
            path = %path.display(),
            error = ?last_error,
            "failed to remove an isolated Browser profile within the cleanup bound"
        );
        Err(BrowserError::ExecutionFailed)
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        // Normal Run completion/cancellation takes the async tree-termination
        // path above. This only covers abrupt manager teardown.
        let _ = self.child.start_kill();
    }
}

struct SnapshotPrecondition {
    id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CdpDownloadState {
    InProgress,
    Completed,
    Canceled,
}

#[derive(Clone, Debug)]
struct CdpDownload {
    guid: String,
    suggested_filename: String,
    state: CdpDownloadState,
    received_bytes: u64,
    total_bytes: Option<u64>,
}

struct CdpClient {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    console: VecDeque<String>,
    dialog: Option<String>,
    downloads: HashMap<String, CdpDownload>,
}

impl CdpClient {
    async fn connect(endpoint: &Url, deadline: Instant) -> Result<Self, BrowserError> {
        if endpoint.scheme() != "ws"
            || endpoint.host_str() != Some("127.0.0.1")
            || endpoint.port().is_none()
        {
            return Err(BrowserError::ExecutionFailed);
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        let (stream, _) = tokio::time::timeout(timeout, connect_async(endpoint.as_str()))
            .await
            .map_err(|_| BrowserError::ExecutionFailed)?
            .map_err(|_| BrowserError::ExecutionFailed)?;
        Ok(Self {
            stream,
            next_id: 0,
            console: VecDeque::new(),
            dialog: None,
            downloads: HashMap::new(),
        })
    }

    async fn call(
        &mut self,
        method: &str,
        params: JsonValue,
        session_id: Option<&str>,
        control: Option<&ToolExecutionControl>,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<JsonValue, BrowserError> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(BrowserError::ExecutionFailed)?;
        let id = self.next_id;
        let mut request = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            request["sessionId"] = JsonValue::String(session_id.to_owned());
        }
        let payload = serde_json::to_string(&request).map_err(|_| BrowserError::ExecutionFailed)?;
        self.send(Message::Text(payload), control, cancellation, deadline)
            .await?;
        loop {
            let value = self.next_json(control, cancellation, deadline).await?;
            if value.get("id").and_then(JsonValue::as_u64) == Some(id) {
                if value.get("error").is_some() {
                    return Err(BrowserError::ExecutionFailed);
                }
                return Ok(value.get("result").cloned().unwrap_or(JsonValue::Null));
            }
            self.record_event(&value);
        }
    }

    async fn send(
        &mut self,
        message: Message,
        control: Option<&ToolExecutionControl>,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(), BrowserError> {
        self.check_wait(control, cancellation, deadline)?;
        tokio::select! {
            _ = cancellation.changed() => Err(BrowserError::Cancelled),
            _ = tokio::time::sleep_until(deadline) => Err(BrowserError::DeadlineExceeded),
            result = self.stream.send(message) => result.map_err(|_| BrowserError::Crashed),
        }
    }

    async fn next_json(
        &mut self,
        control: Option<&ToolExecutionControl>,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<JsonValue, BrowserError> {
        loop {
            self.check_wait(control, cancellation, deadline)?;
            let message = tokio::select! {
                _ = cancellation.changed() => return Err(BrowserError::Cancelled),
                _ = tokio::time::sleep_until(deadline) => return Err(BrowserError::DeadlineExceeded),
                _ = tokio::time::sleep(CDP_POLL_INTERVAL) => continue,
                item = self.stream.next() => item,
            }
            .ok_or(BrowserError::Crashed)?
            .map_err(|_| BrowserError::Crashed)?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).map_err(|_| BrowserError::ExecutionFailed);
                }
                Message::Binary(bytes) => {
                    return serde_json::from_slice(&bytes)
                        .map_err(|_| BrowserError::ExecutionFailed);
                }
                Message::Ping(payload) => {
                    self.stream
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|_| BrowserError::Crashed)?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => return Err(BrowserError::Crashed),
                Message::Frame(_) => {}
            }
        }
    }

    fn check_wait(
        &self,
        control: Option<&ToolExecutionControl>,
        cancellation: &watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<(), BrowserError> {
        if *cancellation.borrow() {
            return Err(BrowserError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::DeadlineExceeded);
        }
        if let Some(control) = control {
            check_control(control)?;
        }
        Ok(())
    }

    fn record_event(&mut self, value: &JsonValue) {
        match value.get("method").and_then(JsonValue::as_str) {
            Some("Runtime.consoleAPICalled") => {
                let text = value
                    .pointer("/params/args")
                    .and_then(JsonValue::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .map(remote_object_text)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_else(|| "console event".to_owned());
                self.push_console(text);
            }
            Some("Log.entryAdded") => {
                if let Some(text) = value
                    .pointer("/params/entry/text")
                    .and_then(JsonValue::as_str)
                {
                    self.push_console(text.to_owned());
                }
            }
            Some("Page.javascriptDialogOpening") => {
                self.dialog = value
                    .pointer("/params/message")
                    .and_then(JsonValue::as_str)
                    .map(|message| bounded_text(message, 500));
            }
            Some("Browser.downloadWillBegin") => {
                let Some(guid) = value.pointer("/params/guid").and_then(JsonValue::as_str) else {
                    return;
                };
                let Some(suggested_filename) = value
                    .pointer("/params/suggestedFilename")
                    .and_then(JsonValue::as_str)
                else {
                    return;
                };
                if !valid_download_guid(guid) {
                    return;
                }
                self.downloads.insert(
                    guid.to_owned(),
                    CdpDownload {
                        guid: guid.to_owned(),
                        suggested_filename: suggested_filename.to_owned(),
                        state: CdpDownloadState::InProgress,
                        received_bytes: 0,
                        total_bytes: None,
                    },
                );
            }
            Some("Browser.downloadProgress") => {
                let Some(guid) = value.pointer("/params/guid").and_then(JsonValue::as_str) else {
                    return;
                };
                if !valid_download_guid(guid) {
                    return;
                }
                let Some(download) = self.downloads.get_mut(guid) else {
                    return;
                };
                if let Some(received) = value
                    .pointer("/params/receivedBytes")
                    .and_then(JsonValue::as_u64)
                {
                    download.received_bytes = received;
                }
                if let Some(total) = value
                    .pointer("/params/totalBytes")
                    .and_then(JsonValue::as_u64)
                {
                    download.total_bytes = Some(total);
                }
                download.state = match value.pointer("/params/state").and_then(JsonValue::as_str) {
                    Some("completed") => CdpDownloadState::Completed,
                    Some("canceled") => CdpDownloadState::Canceled,
                    _ => CdpDownloadState::InProgress,
                };
            }
            _ => {}
        }
    }

    fn push_console(&mut self, text: String) {
        if self.console.len() == MAX_CONSOLE_ENTRIES {
            self.console.pop_front();
        }
        self.console
            .push_back(bounded_text(&text, MAX_CONSOLE_TEXT_CHARS));
    }

    fn console_entries(&self, limit: usize) -> Vec<String> {
        self.console.iter().rev().take(limit).cloned().collect()
    }

    fn clear_downloads(&mut self) {
        self.downloads.clear();
    }

    async fn wait_for_download(
        &mut self,
        maximum_bytes: u64,
        control: Option<&ToolExecutionControl>,
        cancellation: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<CdpDownload, BrowserError> {
        loop {
            if self.downloads.len() > 1 {
                return Err(BrowserError::DownloadRejected);
            }
            if let Some(download) = self.downloads.values().next().cloned() {
                if download.received_bytes > maximum_bytes
                    || download
                        .total_bytes
                        .is_some_and(|total| total > maximum_bytes)
                {
                    self.cancel_download_best_effort(&download.guid).await;
                    return Err(BrowserError::DownloadRejected);
                }
                match download.state {
                    CdpDownloadState::Completed => return Ok(download),
                    CdpDownloadState::Canceled => return Err(BrowserError::DownloadRejected),
                    CdpDownloadState::InProgress => {}
                }
            }
            let value = self.next_json(control, cancellation, deadline).await?;
            self.record_event(&value);
        }
    }

    async fn cancel_download_best_effort(&mut self, guid: &str) {
        let (_sender, mut cancellation) = watch::channel(false);
        let _ = self
            .call(
                "Browser.cancelDownload",
                json!({"guid": guid}),
                None,
                None,
                &mut cancellation,
                Instant::now() + Duration::from_secs(2),
            )
            .await;
    }

    async fn close(&mut self, target_id: &str) -> Result<(), BrowserError> {
        let (_sender, mut cancellation) = watch::channel(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        let _ = self
            .call(
                "Target.closeTarget",
                json!({"targetId": target_id}),
                None,
                None,
                &mut cancellation,
                deadline,
            )
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stream.close(None)).await;
        Ok(())
    }
}

fn navigation_document_ready(state: &str) -> bool {
    matches!(state, "interactive" | "complete")
}

async fn wait_for_devtools_port(
    profile_dir: &TempDir,
    child: &mut Child,
    control: &ToolExecutionControl,
    cancellation: &mut watch::Receiver<bool>,
    deadline: Instant,
) -> Result<u16, BrowserError> {
    let path = profile_dir.path().join("DevToolsActivePort");
    loop {
        check_control(control)?;
        if *cancellation.borrow() {
            return Err(BrowserError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::DeadlineExceeded);
        }
        if child
            .try_wait()
            .map_err(|_| BrowserError::Crashed)?
            .is_some()
        {
            return Err(BrowserError::Crashed);
        }
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Some(port) = content
                .lines()
                .next()
                .and_then(|line| line.parse::<u16>().ok())
        {
            return Ok(port);
        }
        tokio::select! {
            _ = cancellation.changed() => return Err(BrowserError::Cancelled),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

async fn cdp_endpoint(port: u16) -> Result<Url, BrowserError> {
    let endpoint = format!("http://127.0.0.1:{port}/json/version");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| BrowserError::ExecutionFailed)?;
    let body = client
        .get(endpoint)
        .send()
        .await
        .map_err(|_| BrowserError::ExecutionFailed)?
        .error_for_status()
        .map_err(|_| BrowserError::ExecutionFailed)?
        .json::<JsonValue>()
        .await
        .map_err(|_| BrowserError::ExecutionFailed)?;
    let value = body
        .get("webSocketDebuggerUrl")
        .and_then(JsonValue::as_str)
        .ok_or(BrowserError::ExecutionFailed)?;
    let url = Url::parse(value).map_err(|_| BrowserError::ExecutionFailed)?;
    if url.scheme() != "ws" || url.host_str() != Some("127.0.0.1") || url.port() != Some(port) {
        return Err(BrowserError::ExecutionFailed);
    }
    Ok(url)
}

fn discover_browser_binary() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("SYNTHCHAT_BROWSER_BINARY") {
        let path = PathBuf::from(value);
        if executable_file(&path) {
            return Some(path);
        }
    }
    let mut candidates = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(variable) {
                let root = PathBuf::from(root);
                candidates.push(root.join("Google/Chrome/Application/chrome.exe"));
                candidates.push(root.join("Microsoft/Edge/Application/msedge.exe"));
                candidates.push(root.join("Chromium/Application/chrome.exe"));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        candidates.push(PathBuf::from(
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ));
        candidates.push(PathBuf::from(
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        candidates.extend([
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/usr/bin/microsoft-edge"),
        ]);
    }
    candidates
        .into_iter()
        .find(|candidate| executable_file(candidate))
}

fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn check_control(control: &ToolExecutionControl) -> Result<(), BrowserError> {
    match control.check() {
        Ok(()) => Ok(()),
        Err(ToolExecutionControlError::Cancelled) => Err(BrowserError::Cancelled),
        Err(ToolExecutionControlError::DeadlineExceeded) => Err(BrowserError::DeadlineExceeded),
    }
}

fn parse_browser_url(value: &str) -> Result<Url, BrowserError> {
    if value.chars().count() > 4_096 {
        return Err(BrowserError::InvalidArguments);
    }
    let url = Url::parse(value).map_err(|_| BrowserError::InvalidArguments)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(BrowserError::InvalidArguments);
    }
    Ok(url)
}

fn valid_selector(value: String) -> Result<String, BrowserError> {
    if value.is_empty() || value.chars().count() > 1_024 || value.contains('\0') {
        Err(BrowserError::InvalidArguments)
    } else {
        Ok(value)
    }
}

fn valid_snapshot_id(value: String) -> Result<String, BrowserError> {
    if value.starts_with("snapshot_")
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        Ok(value)
    } else {
        Err(BrowserError::InvalidArguments)
    }
}

fn valid_key(value: &str) -> bool {
    matches!(
        value,
        "Enter"
            | "Tab"
            | "Escape"
            | "Backspace"
            | "Delete"
            | "ArrowUp"
            | "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
            | " "
    ) || value.chars().count() == 1
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

fn bounded_text(value: &str, maximum_chars: usize) -> String {
    let mut result = value.chars().take(maximum_chars).collect::<String>();
    if value.chars().count() > maximum_chars {
        result.push_str("...");
    }
    result
}

fn remote_object_text(value: &JsonValue) -> String {
    value
        .get("value")
        .map(JsonValue::to_string)
        .or_else(|| {
            value
                .get("description")
                .and_then(JsonValue::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "[console value]".to_owned())
}

fn current_history_url(value: &JsonValue) -> Option<String> {
    let index = value.get("currentIndex")?.as_u64()? as usize;
    value
        .get("entries")?
        .as_array()?
        .get(index)?
        .get("url")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn project_ax_nodes(tree: &JsonValue) -> Result<Vec<JsonValue>, BrowserError> {
    let input = tree
        .get("nodes")
        .and_then(JsonValue::as_array)
        .ok_or(BrowserError::ExecutionFailed)?;
    let mut projected = Vec::new();
    for node in input.iter().take(MAX_AX_NODES) {
        let value = json!({
            "role": ax_string(node, "role"),
            "name": ax_string(node, "name"),
            "value": ax_string(node, "value"),
            "description": ax_string(node, "description"),
            "ignored": node.get("ignored").and_then(JsonValue::as_bool).unwrap_or(false),
            "backendDomNodeId": node.get("backendDOMNodeId").and_then(JsonValue::as_i64),
        });
        let mut candidate = projected.clone();
        candidate.push(value.clone());
        if serde_json::to_vec(&candidate)
            .map_err(|_| BrowserError::ExecutionFailed)?
            .len()
            > MAX_SNAPSHOT_BYTES.saturating_sub(1_024)
        {
            break;
        }
        projected.push(value);
    }
    Ok(projected)
}

fn ax_string(node: &JsonValue, key: &str) -> Option<String> {
    node.get(key)
        .and_then(|value| value.get("value"))
        .and_then(JsonValue::as_str)
        .map(|value| bounded_text(value, MAX_AX_TEXT_CHARS))
}

fn bounded_snapshot_value(
    snapshot_id: &str,
    url: &str,
    title: Option<&str>,
    nodes: Vec<JsonValue>,
) -> Result<JsonValue, BrowserError> {
    let value = json!({
        "snapshotId": snapshot_id,
        "url": bounded_text(url, 4_096),
        "title": title.map(|title| bounded_text(title, 500)),
        "nodes": nodes,
    });
    if serde_json::to_vec(&value)
        .map_err(|_| BrowserError::ExecutionFailed)?
        .len()
        > MAX_SNAPSHOT_BYTES
    {
        return Err(BrowserError::ExecutionFailed);
    }
    Ok(value)
}

fn bounded_json_value(value: JsonValue, maximum_bytes: usize) -> Result<JsonValue, BrowserError> {
    let encoded = serde_json::to_vec(&value).map_err(|_| BrowserError::ExecutionFailed)?;
    if encoded.len() > maximum_bytes {
        return Err(BrowserError::ExecutionFailed);
    }
    Ok(value)
}

fn parse_input<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, BrowserError> {
    if raw.len() > 64 * 1024 {
        return Err(BrowserError::InvalidArguments);
    }
    serde_json::from_str(raw).map_err(|_| BrowserError::InvalidArguments)
}

fn parse_empty_input(raw: &str) -> Result<(), BrowserError> {
    let value: JsonValue = parse_input(raw)?;
    if value.as_object().is_some_and(|object| object.is_empty()) {
        Ok(())
    } else {
        Err(BrowserError::InvalidArguments)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NavigateInput {
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClickInput {
    selector: String,
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TypeInput {
    selector: String,
    text: String,
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScrollInput {
    #[serde(default)]
    delta_x: i64,
    delta_y: i64,
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotInput {
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PressInput {
    key: String,
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisionInput {
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConsoleInput {
    #[serde(default = "default_console_limit")]
    limit: usize,
}

fn default_console_limit() -> usize {
    20
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CdpInput {
    method: String,
    expression: String,
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DialogInput {
    action: String,
    #[serde(default)]
    prompt_text: Option<String>,
    snapshot_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tokio_tungstenite::accept_async;

    #[test]
    fn production_policy_rejects_private_and_special_addresses() {
        let policy = EgressPolicy::production();
        for address in [
            "127.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "169.254.1.1".parse().unwrap(),
            "192.168.1.1".parse().unwrap(),
            "::1".parse().unwrap(),
            "fc00::1".parse().unwrap(),
            "fe80::1".parse().unwrap(),
        ] {
            assert!(!policy.permits_address(address));
        }
        assert!(policy.permits_address("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn navigation_continues_once_the_dom_is_available() {
        assert!(!navigation_document_ready("loading"));
        assert!(navigation_document_ready("interactive"));
        assert!(navigation_document_ready("complete"));
    }

    #[test]
    fn browser_actions_require_bounded_current_snapshot_for_mutations() {
        let action = BrowserAction::parse(
            "browser_click",
            r#"{"selector":"button","snapshotId":"snapshot_abc123"}"#,
        )
        .unwrap();
        assert_eq!(action.requires_snapshot(), Some("snapshot_abc123"));
        assert!(matches!(
            BrowserAction::parse(
                "browser_click",
                r#"{"selector":"button","snapshotId":"stale"}"#
            ),
            Err(BrowserError::InvalidArguments)
        ));
        assert!(matches!(
            BrowserAction::parse(
                "browser_cdp",
                r#"{"method":"Browser.setDownloadBehavior","expression":"1","snapshotId":"snapshot_abc123"}"#
            ),
            Err(BrowserError::InvalidArguments)
        ));
        let download = BrowserAction::parse(
            "browser_download",
            r#"{"selector":"a#report","snapshotId":"snapshot_abc123"}"#,
        )
        .unwrap();
        assert_eq!(download.requires_snapshot(), Some("snapshot_abc123"));
        assert!(matches!(
            BrowserAction::parse(
                "browser_download",
                r#"{"selector":"a#report","snapshotId":"stale"}"#
            ),
            Err(BrowserError::InvalidArguments)
        ));
    }

    #[test]
    fn isolated_download_store_projects_only_bounded_metadata() {
        let profile = tempfile::tempdir().unwrap();
        let store = DownloadStore::create(profile.path()).unwrap();
        let guid = "0123456789abcdef";
        fs::write(store.path.join(guid), b"safely downloaded text\n").unwrap();

        let projection = store
            .project_completed(guid, "report.txt", MAX_DOWNLOAD_BYTES)
            .unwrap();
        assert_eq!(projection.name, "report.txt");
        assert_eq!(projection.mime_type, "text/plain");
        let public = projection.public_value();
        assert_eq!(public["scan"]["contentExposed"], false);
        assert_eq!(public["scan"]["workspaceImported"], false);
        assert!(
            !public
                .to_string()
                .contains(profile.path().to_str().unwrap())
        );
        assert!(!public.to_string().contains("safely downloaded text"));

        store.cleanup_contents().unwrap();
        assert!(store.entry_names().unwrap().is_empty());
    }

    #[test]
    fn isolated_download_store_rejects_unsafe_names_and_oversized_content() {
        for name in [
            "../report.txt",
            "dir/report.txt",
            "dir\\report.txt",
            ".hidden.txt",
        ] {
            assert!(matches!(
                valid_download_filename(name),
                Err(BrowserError::DownloadRejected)
            ));
        }
        let profile = tempfile::tempdir().unwrap();
        let store = DownloadStore::create(profile.path()).unwrap();
        let guid = "fedcba9876543210";
        fs::write(store.path.join(guid), b"too long").unwrap();
        assert!(matches!(
            store.project_completed(guid, "report.txt", 2),
            Err(BrowserError::DownloadRejected)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn isolated_download_store_never_follows_a_link() {
        use std::os::unix::fs::symlink;

        let profile = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"outside content").unwrap();
        let store = DownloadStore::create(profile.path()).unwrap();
        let guid = "1111111111111111";
        symlink(outside.path(), store.path.join(guid)).unwrap();

        assert!(matches!(
            store.project_completed(guid, "report.txt", MAX_DOWNLOAD_BYTES),
            Err(BrowserError::DownloadRejected)
        ));
        assert_eq!(fs::read(outside.path()).unwrap(), b"outside content");
    }

    #[tokio::test]
    async fn cdp_client_speaks_json_rpc_to_a_deterministic_fixture() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let request: JsonValue = serde_json::from_str(&request).unwrap();
            socket
                .send(Message::Text(
                    json!({"id": request["id"], "result": {"ok": true}}).to_string(),
                ))
                .await
                .unwrap();
        });
        let endpoint = Url::parse(&format!(
            "ws://127.0.0.1:{}/devtools/browser/test",
            address.port()
        ))
        .unwrap();
        let mut cdp = CdpClient::connect(&endpoint, Instant::now() + Duration::from_secs(3))
            .await
            .unwrap();
        let (_sender, mut cancellation) = watch::channel(false);
        let result = cdp
            .call(
                "Browser.getVersion",
                json!({}),
                None,
                None,
                &mut cancellation,
                Instant::now() + Duration::from_secs(3),
            )
            .await
            .unwrap();
        assert_eq!(result, json!({"ok": true}));
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn cdp_download_events_drive_an_isolated_metadata_projection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let guid = "a1b2c3d4e5f60708";
        let fixture = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let request: JsonValue = serde_json::from_str(&request).unwrap();
            socket
                .send(Message::Text(
                    json!({"id": request["id"], "result": {}}).to_string(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "method": "Browser.downloadWillBegin",
                        "params": {"guid": guid, "suggestedFilename": "fixture.txt", "url": "https://example.invalid/fixture.txt"}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "method": "Browser.downloadProgress",
                        "params": {"guid": guid, "state": "completed", "receivedBytes": 17, "totalBytes": 17}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let endpoint = Url::parse(&format!(
            "ws://127.0.0.1:{}/devtools/browser/test",
            address.port()
        ))
        .unwrap();
        let mut cdp = CdpClient::connect(&endpoint, Instant::now() + Duration::from_secs(3))
            .await
            .unwrap();
        let (_sender, mut cancellation) = watch::channel(false);
        cdp.call(
            "Browser.setDownloadBehavior",
            json!({"behavior": "allowAndName"}),
            None,
            None,
            &mut cancellation,
            Instant::now() + Duration::from_secs(3),
        )
        .await
        .unwrap();
        let profile = tempfile::tempdir().unwrap();
        let store = DownloadStore::create(profile.path()).unwrap();
        fs::write(store.path.join(guid), b"fixture download\n").unwrap();
        let observed = cdp
            .wait_for_download(
                MAX_DOWNLOAD_BYTES,
                None,
                &mut cancellation,
                Instant::now() + Duration::from_secs(3),
            )
            .await
            .unwrap();
        assert_eq!(observed.guid, guid);
        let projection = store
            .project_completed(
                &observed.guid,
                &observed.suggested_filename,
                MAX_DOWNLOAD_BYTES,
            )
            .unwrap();
        assert_eq!(projection.mime_type, "text/plain");
        assert!(
            !projection
                .public_value()
                .to_string()
                .contains(profile.path().to_str().unwrap())
        );
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn real_headless_chromium_navigates_local_fixture_and_returns_ax_snapshot() {
        let Some(executable) = discover_browser_binary() else {
            return;
        };
        let fixture = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = fixture.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = fixture.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!doctype html><title>Browser fixture</title><main><button>Continue</button></main>")
                .await
                .unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let manager = BrowserManager::with_test_binary(home.path(), executable);
        let owner = BrowserOwner::new("profile", "session", "run");
        let profile_root = browser_profile_root(home.path(), &owner).unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        let control = ToolExecutionControl::new(deadline.into_std());
        let (_sender, cancellation) = watch::channel(false);
        manager
            .execute(
                owner.clone(),
                BrowserAction::Navigate {
                    url: Url::parse(&format!("http://127.0.0.1:{}/", address.port())).unwrap(),
                },
                control.clone(),
                cancellation.clone(),
                deadline,
            )
            .await
            .unwrap();
        let snapshot = manager
            .execute(
                owner.clone(),
                BrowserAction::Snapshot,
                control,
                cancellation,
                deadline,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.value["title"], "Browser fixture");
        assert!(
            snapshot.value["nodes"]
                .as_array()
                .is_some_and(|nodes| !nodes.is_empty())
        );
        manager.cleanup_run("profile", "run").await;
        let remaining = fs::read_dir(profile_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(remaining.is_empty(), "remaining profiles: {remaining:?}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn real_headless_chromium_download_stays_isolated_and_returns_metadata_only() {
        let Some(executable) = discover_browser_binary() else {
            return;
        };
        let fixture = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = fixture.local_addr().unwrap();
        let (shutdown, mut shutdown_requested) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_requested => break,
                    accepted = fixture.accept() => {
                        let Ok((mut stream, _)) = accepted else { break; };
                        tokio::spawn(async move {
                            let mut request = [0_u8; 4096];
                            let read = stream.read(&mut request).await.unwrap_or(0);
                            let request = String::from_utf8_lossy(&request[..read]);
                            let response = if request.starts_with("GET /report.txt ") {
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=report.txt\r\nContent-Length: 17\r\nConnection: close\r\n\r\ndownload fixture\n".as_slice()
                            } else {
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!doctype html><title>Download fixture</title><a id=\"download\" href=\"/report.txt\">Download</a>".as_slice()
                            };
                            let _ = stream.write_all(response).await;
                        });
                    }
                }
            }
        });
        let home = tempfile::tempdir().unwrap();
        let manager = BrowserManager::with_test_binary(home.path(), executable);
        let owner = BrowserOwner::new("profile", "session", "run");
        let profile_root = browser_profile_root(home.path(), &owner).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let control = ToolExecutionControl::new(deadline.into_std());
        let (_sender, cancellation) = watch::channel(false);
        manager
            .execute(
                owner.clone(),
                BrowserAction::Navigate {
                    url: Url::parse(&format!("http://127.0.0.1:{}/", address.port())).unwrap(),
                },
                control.clone(),
                cancellation.clone(),
                deadline,
            )
            .await
            .unwrap();
        let snapshot = manager
            .execute(
                owner.clone(),
                BrowserAction::Snapshot,
                control.clone(),
                cancellation.clone(),
                deadline,
            )
            .await
            .unwrap();
        let snapshot_id = snapshot.value["snapshotId"].as_str().unwrap().to_owned();
        let downloaded = manager
            .execute(
                owner,
                BrowserAction::Download {
                    selector: "#download".to_owned(),
                    snapshot_id,
                },
                control,
                cancellation,
                deadline,
            )
            .await
            .unwrap();
        assert_eq!(downloaded.value["download"]["name"], "report.txt");
        assert_eq!(downloaded.value["download"]["mimeType"], "text/plain");
        assert_eq!(
            downloaded.value["download"]["scan"]["contentExposed"],
            false
        );
        assert_eq!(
            downloaded.value["download"]["scan"]["workspaceImported"],
            false
        );
        assert!(
            !downloaded
                .value
                .to_string()
                .contains(home.path().to_str().unwrap())
        );
        manager.cleanup_run("profile", "run").await;
        let remaining = fs::read_dir(profile_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(remaining.is_empty(), "remaining profiles: {remaining:?}");
        let _ = shutdown.send(());
        server.await.unwrap();
    }
}
