use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin},
    sync::{Mutex as AsyncMutex, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

use crate::{
    sessions::{
        SessionService,
        process_store::{
            AsyncToolDeliveryRequest, CreateProcess, ProcessOwner, ProcessRecord, ProcessStatus,
            ProcessStoreError, ProcessTransition,
        },
    },
    tools::{ToolExecutionControl, ToolExecutionControlError},
};

use super::{
    guardian::{encode_process_guardian_stdin, encode_process_guardian_stdin_close},
    output::{CaptureMode, OutputRedactor, ProcessOutputCapture, ProcessStream},
    platform,
    shell::{LocalShell, resolve_workspace_cwd},
};

const ACTIVE_PROCESS_LIMIT: usize = 64;
const FINISHED_PROCESS_TTL: Duration = Duration::minutes(30);
const FOREGROUND_OUTPUT_LIMIT: usize = 50_000;
const POLL_OUTPUT_CHARS: usize = 1_000;
const WAIT_OUTPUT_CHARS: usize = 2_000;
const SUPERVISOR_CAPACITY: usize = 16;
const STDIN_CAPACITY: usize = 4;
const STDIN_WRITE_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const PROCESS_CONTROL_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const OUTPUT_DRAIN_TIMEOUT: StdDuration = StdDuration::from_secs(2);

#[derive(Clone, Debug)]
pub(crate) struct ProcessExecutionContext {
    pub(crate) profile_id: String,
    pub(crate) session_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) creator_run_id: String,
    pub(crate) call_id: String,
}

impl ProcessExecutionContext {
    fn owner(&self) -> ProcessOwner {
        ProcessOwner {
            profile_id: self.profile_id.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalExecutionRequest {
    pub(crate) command: String,
    pub(crate) background: bool,
    pub(crate) timeout: StdDuration,
    pub(crate) workdir: Option<PathBuf>,
}

struct PreparedTerminalCommand {
    command_text: String,
    cwd: PathBuf,
    shell: LocalShell,
    redactor: Arc<SecretMasker>,
    control: ToolExecutionControl,
}

/// Owns the exact containment boundary for one foreground guardian. Async
/// cancellation should use the bounded cleanup methods; dropping the future
/// still synchronously tears down the owned Job/process group.
struct ForegroundProcess {
    pid: u32,
    #[cfg(not(target_os = "windows"))]
    identity: String,
    child: Child,
    lifetime: Option<platform::ProcessLifetime>,
    settled: bool,
}

impl ForegroundProcess {
    fn new(pid: u32, identity: String, child: Child, lifetime: platform::ProcessLifetime) -> Self {
        #[cfg(target_os = "windows")]
        let _ = identity;
        Self {
            pid,
            #[cfg(not(target_os = "windows"))]
            identity,
            child,
            lifetime: Some(lifetime),
            settled: false,
        }
    }

    async fn terminate(&mut self) -> io::Result<()> {
        let lifetime = self
            .lifetime
            .as_ref()
            .ok_or_else(|| io::Error::other("foreground process lifetime is unavailable"))?;
        platform::terminate_tree(self.pid, &mut self.child, lifetime).await
    }

    fn finish_after_root_exit(&mut self) -> io::Result<()> {
        let cleanup = self
            .lifetime
            .take()
            .ok_or_else(|| io::Error::other("foreground process lifetime is unavailable"))
            .and_then(|lifetime| platform::finish_after_root_exit(self.pid, lifetime));
        self.settled = true;
        cleanup
    }
}

impl Drop for ForegroundProcess {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        #[cfg(target_os = "windows")]
        if let Some(lifetime) = self.lifetime.as_ref() {
            let _ = platform::terminate_lifetime_now(lifetime);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = platform::terminate_tree_now(self.pid, &self.identity);
        let _ = self.child.start_kill();
        self.lifetime.take();
        self.settled = true;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalExecutionResult {
    Foreground {
        output: String,
        exit_code: i32,
        error: Option<String>,
    },
    Background {
        process_id: String,
        pid: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessView {
    pub(crate) record: ProcessRecord,
    pub(crate) output: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessLog {
    pub(crate) process_id: String,
    pub(crate) output: String,
    pub(crate) total_lines: u64,
    pub(crate) showing: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessWaitStatus {
    Exited,
    Timeout,
    Interrupted,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessWaitResult {
    pub(crate) status: ProcessWaitStatus,
    pub(crate) view: Option<ProcessView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessMutationStatus {
    Killed,
    AlreadyExited,
    Written,
    Submitted,
    Closed,
    NotFound,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessMutationResult {
    pub(crate) status: ProcessMutationStatus,
    pub(crate) view: Option<ProcessView>,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ProcessExecutionError {
    #[error("terminal requires a registered Workspace")]
    WorkspaceRequired,
    #[error("terminal workdir is invalid or changed")]
    InvalidWorkdir,
    #[error("the compatible local shell is unavailable")]
    ShellUnavailable,
    #[error("process storage is unavailable")]
    StorageUnavailable,
    #[error("the background process limit was reached")]
    ProcessLimitReached,
    #[error("process could not be started")]
    SpawnFailed,
    #[error("process is not available to this owner")]
    NotFound,
    #[error("process stdin is unavailable")]
    StdinUnavailable,
    #[error("process operation failed")]
    OperationFailed,
    #[error("tool execution was cancelled")]
    Cancelled,
    #[error("tool execution deadline was exceeded")]
    DeadlineExceeded,
}

#[derive(Clone)]
pub(crate) struct ProcessManager {
    inner: Arc<ProcessManagerInner>,
}

struct ProcessManagerInner {
    sessions: Arc<SessionService>,
    entries: Arc<Mutex<HashMap<String, Arc<ManagedProcess>>>>,
    launch_gate: AsyncMutex<()>,
    shutting_down: AtomicBool,
}

struct ManagedProcess {
    pid: u32,
    identity: String,
    output: ProcessOutputCapture,
    redactor: Arc<SecretMasker>,
    status: watch::Receiver<ProcessRecord>,
    supervisor: mpsc::Sender<SupervisorCommand>,
}

enum SupervisorCommand {
    CommitLaunch {
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    Stop {
        cause: ProcessStopCause,
        reply: oneshot::Sender<Result<ProcessRecord, SupervisorError>>,
    },
    Write {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    Close {
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
}

#[derive(Clone, Copy, Debug)]
enum ProcessStopCause {
    Owner,
    BackendShutdown,
}

impl ProcessStopCause {
    fn transition(self) -> ProcessTransition {
        match self {
            Self::Owner => ProcessTransition::Killed {
                exit_code: None,
                completion_reason: "process killed by owner".to_owned(),
                termination_source: "process_tool".to_owned(),
            },
            Self::BackendShutdown => ProcessTransition::Killed {
                exit_code: None,
                completion_reason: "process stopped during backend shutdown".to_owned(),
                termination_source: "backend_shutdown".to_owned(),
            },
        }
    }
}

enum StdinCommand {
    Write {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    Close {
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
}

#[derive(Clone, Copy, Debug)]
enum SupervisorError {
    StdinUnavailable,
    OperationFailed,
    StorageUnavailable,
}

impl Drop for ProcessManagerInner {
    fn drop(&mut self) {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in entries.values() {
            if !entry.status.borrow().status.is_terminal() {
                let _ = platform::terminate_tree_now(entry.pid, &entry.identity);
            }
        }
        drop(entries);
        if let Ok(records) = self.sessions.list_recovery_candidates() {
            for record in records {
                if let Some((pid, identity)) = record.pid.zip(record.process_identity.as_deref()) {
                    let _ = platform::terminate_tree_now(pid, identity);
                }
            }
        }
    }
}

impl ProcessManager {
    pub(crate) fn new(sessions: Arc<SessionService>) -> Self {
        let manager = Self {
            inner: Arc::new(ProcessManagerInner {
                sessions,
                entries: Arc::new(Mutex::new(HashMap::new())),
                launch_gate: AsyncMutex::new(()),
                shutting_down: AtomicBool::new(false),
            }),
        };
        manager.reconcile_recovery_candidates();
        manager
    }

    #[cfg(test)]
    pub(crate) async fn execute_terminal(
        &self,
        context: ProcessExecutionContext,
        request: TerminalExecutionRequest,
        secrets: Vec<SecretString>,
        control: ToolExecutionControl,
        cancellation: watch::Receiver<bool>,
        run_deadline: Instant,
    ) -> Result<TerminalExecutionResult, ProcessExecutionError> {
        self.execute_terminal_with_async_delivery(
            context,
            request,
            secrets,
            control,
            cancellation,
            run_deadline,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_terminal_with_async_delivery(
        &self,
        context: ProcessExecutionContext,
        request: TerminalExecutionRequest,
        secrets: Vec<SecretString>,
        control: ToolExecutionControl,
        cancellation: watch::Receiver<bool>,
        run_deadline: Instant,
        async_delivery: Option<AsyncToolDeliveryRequest>,
    ) -> Result<TerminalExecutionResult, ProcessExecutionError> {
        self.prune_finished().await;
        check_control(&control)?;
        let workspace_root = context
            .workspace_root
            .as_deref()
            .ok_or(ProcessExecutionError::WorkspaceRequired)?;
        let cwd = resolve_workspace_cwd(workspace_root, request.workdir.as_deref())
            .await
            .map_err(|_| ProcessExecutionError::InvalidWorkdir)?;
        let shell = LocalShell::discover()
            .await
            .map_err(|_| ProcessExecutionError::ShellUnavailable)?;
        check_control(&control)?;

        let redactor = Arc::new(SecretMasker::new(secrets));
        let prepared = PreparedTerminalCommand {
            command_text: request.command,
            cwd,
            shell,
            redactor,
            control,
        };
        if request.background {
            self.start_background(
                context,
                prepared,
                cancellation,
                run_deadline,
                async_delivery,
            )
            .await
        } else {
            if async_delivery.is_some() {
                return Err(ProcessExecutionError::SpawnFailed);
            }
            let mut cancellation = cancellation;
            execute_foreground(
                &self.inner,
                prepared,
                request.timeout,
                &mut cancellation,
                run_deadline,
            )
            .await
        }
    }

    pub(crate) async fn commit_launch(
        &self,
        owner: ProcessOwner,
        process_id: &str,
    ) -> Result<(), ProcessExecutionError> {
        let record = self.load(&owner, process_id).await?;
        if record.status == ProcessStatus::Exited {
            // A short-lived background command may exit after its durable row is
            // created but before the Run commits the launch lease. It is already
            // safely contained and can still be delivered from the journal.
            return Ok(());
        }
        if record.status.is_terminal() {
            return Err(ProcessExecutionError::OperationFailed);
        }
        let entry = self
            .entry(process_id)
            .ok_or(ProcessExecutionError::OperationFailed)?;
        let (reply, response) = oneshot::channel();
        tokio::time::timeout(
            PROCESS_CONTROL_TIMEOUT,
            entry
                .supervisor
                .send(SupervisorCommand::CommitLaunch { reply }),
        )
        .await
        .map_err(|_| ProcessExecutionError::OperationFailed)?
        .map_err(|_| ProcessExecutionError::OperationFailed)?;
        tokio::time::timeout(PROCESS_CONTROL_TIMEOUT, response)
            .await
            .map_err(|_| ProcessExecutionError::OperationFailed)?
            .map_err(|_| ProcessExecutionError::OperationFailed)?
            .map_err(|error| match error {
                SupervisorError::StorageUnavailable => ProcessExecutionError::StorageUnavailable,
                SupervisorError::StdinUnavailable | SupervisorError::OperationFailed => {
                    ProcessExecutionError::OperationFailed
                }
            })
    }

    /// Stop every terminal process which remains durable as `starting` or
    /// `running`, including detached recovery candidates from an older backend.
    /// Stop all terminal processes without extending the caller's absolute
    /// shutdown deadline at each reconciliation phase.
    pub(crate) async fn shutdown_all_until(&self, deadline: Instant) {
        let deadline = tokio::time::Instant::from_std(deadline);
        {
            let _launch_gate =
                match tokio::time::timeout_at(deadline, self.inner.launch_gate.lock()).await {
                    Ok(launch_gate) => launch_gate,
                    Err(_) => {
                        self.inner.shutting_down.store(true, Ordering::Release);
                        tracing::warn!("timed out closing the terminal launch gate");
                        return;
                    }
                };
            self.inner.shutting_down.store(true, Ordering::Release);
        }

        let candidates = match tokio::time::timeout_at(
            deadline,
            store_call({
                let sessions = self.inner.sessions.clone();
                move || sessions.list_recovery_candidates()
            }),
        )
        .await
        {
            Ok(Ok(candidates)) => candidates,
            Ok(Err(error)) => {
                tracing::warn!(
                    ?error,
                    "failed to enumerate terminal processes during shutdown"
                );
                Vec::new()
            }
            Err(_) => {
                tracing::warn!("timed out enumerating terminal processes during shutdown");
                Vec::new()
            }
        };

        let mut workers = JoinSet::new();
        for candidate in candidates.iter().cloned() {
            let manager = self.clone();
            workers.spawn(async move {
                manager.shutdown_candidate(candidate).await;
            });
        }
        if tokio::time::timeout_at(deadline, async {
            while workers.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            tracing::warn!("timed out stopping managed terminal processes");
        }

        // A supervisor can be between physical termination and persistence
        // when its bounded worker expires. Re-scan and force exact-identity
        // termination so no recovery candidate survives object teardown.
        let remaining = match tokio::time::timeout_at(
            deadline,
            store_call({
                let sessions = self.inner.sessions.clone();
                move || sessions.list_recovery_candidates()
            }),
        )
        .await
        {
            Ok(Ok(records)) => records,
            _ => candidates,
        };
        let mut fallback_workers = JoinSet::new();
        for record in remaining.iter().cloned() {
            let manager = self.clone();
            fallback_workers.spawn(async move {
                manager.force_shutdown_candidate(record).await;
            });
        }
        if tokio::time::timeout_at(deadline, async {
            while fallback_workers.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            fallback_workers.abort_all();
            while fallback_workers.join_next().await.is_some() {}
            for record in &remaining {
                if let Some((pid, identity)) = record.pid.zip(record.process_identity.as_deref()) {
                    let _ = platform::terminate_tree_now(pid, identity);
                }
            }
            tracing::warn!("timed out reconciling terminal processes during shutdown");
        }
    }

    /// Release retained output only after shutdown delivery settlement has
    /// consumed it. Clearing entries earlier can turn a matched watch into a
    /// false `watch_missed` notification.
    pub(crate) fn release_shutdown_resources(&self) {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    async fn shutdown_candidate(&self, candidate: ProcessRecord) {
        let owner = ProcessOwner {
            profile_id: candidate.profile_id.clone(),
            session_id: candidate.session_id.clone(),
        };
        let current = match self.load(&owner, &candidate.process_id).await {
            Ok(record) => record,
            Err(_) => candidate,
        };
        if current.status.is_terminal() {
            return;
        }
        if let Some(entry) = self.entry(&current.process_id) {
            match stop_managed_process(entry, ProcessStopCause::BackendShutdown).await {
                Ok(_) => return,
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        process_id = %current.process_id,
                        "supervisor stop failed during backend shutdown"
                    );
                }
            }
        }
        self.force_shutdown_candidate(current).await;
    }

    async fn force_shutdown_candidate(&self, record: ProcessRecord) {
        if record.status.is_terminal() {
            return;
        }
        let owner = ProcessOwner {
            profile_id: record.profile_id.clone(),
            session_id: record.session_id.clone(),
        };
        let mut terminated =
            if let Some((pid, identity)) = record.pid.zip(record.process_identity.as_deref()) {
                if platform::identity_matches(pid, identity) {
                    tokio::time::timeout(
                        PROCESS_CONTROL_TIMEOUT,
                        platform::terminate_detached_tree(pid, identity),
                    )
                    .await
                    .is_ok_and(|result| result.is_ok())
                } else {
                    true
                }
            } else {
                false
            };
        if !terminated
            && let Some((pid, identity)) = record.pid.zip(record.process_identity.as_deref())
            && platform::identity_matches(pid, identity)
        {
            terminated = platform::terminate_tree_now(pid, identity);
        }

        let latest = self
            .load(&owner, &record.process_id)
            .await
            .unwrap_or(record);
        if latest.status.is_terminal() {
            return;
        }
        let transition = if terminated && latest.status == ProcessStatus::Running {
            ProcessStopCause::BackendShutdown.transition()
        } else {
            ProcessTransition::Lost {
                completion_reason: "process could not be stopped during backend shutdown"
                    .to_owned(),
                termination_source: "backend_shutdown".to_owned(),
            }
        };
        let process_id = latest.process_id.clone();
        let _ = self
            .transition(owner, &process_id, latest.status, transition)
            .await;
    }

    pub(crate) async fn list(
        &self,
        owner: ProcessOwner,
    ) -> Result<Vec<ProcessView>, ProcessExecutionError> {
        self.prune_finished().await;
        let records = store_call({
            let sessions = self.inner.sessions.clone();
            let owner = owner.clone();
            move || sessions.list_processes(&owner)
        })
        .await?;
        let mut views = Vec::with_capacity(records.len());
        for record in records {
            let record = self.refresh_detached(record).await?;
            views.push(self.view(record, usize::MAX));
        }
        Ok(views)
    }

    pub(crate) async fn poll(
        &self,
        owner: ProcessOwner,
        process_id: &str,
    ) -> Result<ProcessView, ProcessExecutionError> {
        let record = self.load(&owner, process_id).await?;
        let record = self.refresh_detached(record).await?;
        Ok(self.view(record, POLL_OUTPUT_CHARS))
    }

    pub(crate) async fn log(
        &self,
        owner: ProcessOwner,
        process_id: &str,
        offset: u64,
        limit: u64,
    ) -> Result<ProcessLog, ProcessExecutionError> {
        let record = self.load(&owner, process_id).await?;
        let output = self.output_for(&record.process_id, usize::MAX);
        let lines = output.lines().collect::<Vec<_>>();
        let total_lines = u64::try_from(lines.len()).unwrap_or(u64::MAX);
        let limit = usize::try_from(limit)
            .unwrap_or(usize::MAX)
            .min(lines.len());
        let start = if offset == 0 {
            lines.len().saturating_sub(limit)
        } else {
            usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(lines.len())
        };
        let end = start.saturating_add(limit).min(lines.len());
        let selected = lines[start..end].join("\n");
        let showing = if start == end {
            "0 lines".to_owned()
        } else {
            format!("lines {}-{}", start + 1, end)
        };
        Ok(ProcessLog {
            process_id: record.process_id,
            output: selected,
            total_lines,
            showing,
        })
    }

    pub(crate) async fn wait(
        &self,
        owner: ProcessOwner,
        process_id: &str,
        timeout: Option<StdDuration>,
        control: ToolExecutionControl,
        mut cancellation: watch::Receiver<bool>,
        run_deadline: Instant,
    ) -> Result<ProcessWaitResult, ProcessExecutionError> {
        let initial = match self.load(&owner, process_id).await {
            Ok(record) => record,
            Err(ProcessExecutionError::NotFound) => {
                return Ok(ProcessWaitResult {
                    status: ProcessWaitStatus::NotFound,
                    view: None,
                });
            }
            Err(error) => return Err(error),
        };
        if initial.status.is_terminal() {
            return Ok(ProcessWaitResult {
                status: ProcessWaitStatus::Exited,
                view: Some(self.view(initial, WAIT_OUTPUT_CHARS)),
            });
        }

        let action_deadline = timeout
            .and_then(|timeout| Instant::now().checked_add(timeout))
            .unwrap_or(run_deadline);
        let effective_deadline = action_deadline.min(run_deadline);
        let handle = self.entry(process_id);
        if let Some(handle) = handle {
            let mut status = handle.status.clone();
            loop {
                let current = status.borrow().clone();
                if current.status.is_terminal() {
                    return Ok(ProcessWaitResult {
                        status: ProcessWaitStatus::Exited,
                        view: Some(self.view(current, WAIT_OUTPUT_CHARS)),
                    });
                }
                tokio::select! {
                    changed = status.changed() => {
                        if changed.is_err() {
                            return Err(ProcessExecutionError::OperationFailed);
                        }
                    }
                    changed = cancellation.changed() => {
                        if changed.is_err() || *cancellation.borrow() {
                            return Ok(ProcessWaitResult {
                                status: ProcessWaitStatus::Interrupted,
                                view: Some(self.view(current, WAIT_OUTPUT_CHARS)),
                            });
                        }
                    }
                    _ = tokio::time::sleep_until(effective_deadline.into()) => {
                        if effective_deadline >= run_deadline && action_deadline >= run_deadline {
                            check_control(&control)?;
                            return Err(ProcessExecutionError::DeadlineExceeded);
                        }
                        return Ok(ProcessWaitResult {
                            status: ProcessWaitStatus::Timeout,
                            view: Some(self.view(current, WAIT_OUTPUT_CHARS)),
                        });
                    }
                }
            }
        }

        loop {
            let current = self.load(&owner, process_id).await?;
            let current = self.refresh_detached(current).await?;
            if current.status.is_terminal() {
                return Ok(ProcessWaitResult {
                    status: ProcessWaitStatus::Exited,
                    view: Some(self.view(current, WAIT_OUTPUT_CHARS)),
                });
            }
            tokio::select! {
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() {
                        return Ok(ProcessWaitResult {
                            status: ProcessWaitStatus::Interrupted,
                            view: Some(self.view(current, WAIT_OUTPUT_CHARS)),
                        });
                    }
                }
                _ = tokio::time::sleep_until(effective_deadline.into()) => {
                    if effective_deadline >= run_deadline && action_deadline >= run_deadline {
                        check_control(&control)?;
                        return Err(ProcessExecutionError::DeadlineExceeded);
                    }
                    return Ok(ProcessWaitResult {
                        status: ProcessWaitStatus::Timeout,
                        view: Some(self.view(current, WAIT_OUTPUT_CHARS)),
                    });
                }
                _ = tokio::time::sleep(StdDuration::from_millis(250)) => {}
            }
        }
    }

    pub(crate) async fn kill(
        &self,
        owner: ProcessOwner,
        process_id: &str,
    ) -> Result<ProcessMutationResult, ProcessExecutionError> {
        let record = match self.load(&owner, process_id).await {
            Ok(record) => record,
            Err(ProcessExecutionError::NotFound) => return Ok(not_found_mutation()),
            Err(error) => return Err(error),
        };
        if record.status.is_terminal() {
            return Ok(ProcessMutationResult {
                status: ProcessMutationStatus::AlreadyExited,
                view: Some(self.view(record, WAIT_OUTPUT_CHARS)),
            });
        }
        if let Some(entry) = self.entry(process_id) {
            match stop_managed_process(entry, ProcessStopCause::Owner).await {
                Ok(record) => Ok(ProcessMutationResult {
                    status: if record.status == ProcessStatus::Killed {
                        ProcessMutationStatus::Killed
                    } else {
                        ProcessMutationStatus::AlreadyExited
                    },
                    view: Some(self.view(record, WAIT_OUTPUT_CHARS)),
                }),
                Err(SupervisorError::StorageUnavailable) => {
                    Err(ProcessExecutionError::StorageUnavailable)
                }
                Err(_) => {
                    let latest = self.load(&owner, process_id).await?;
                    Ok(ProcessMutationResult {
                        status: if latest.status.is_terminal() {
                            ProcessMutationStatus::AlreadyExited
                        } else {
                            ProcessMutationStatus::Error
                        },
                        view: Some(self.view(latest, WAIT_OUTPUT_CHARS)),
                    })
                }
            }
        } else {
            self.kill_detached(owner, record, process_id, ProcessStopCause::Owner)
                .await
        }
    }

    async fn kill_detached(
        &self,
        owner: ProcessOwner,
        record: ProcessRecord,
        process_id: &str,
        cause: ProcessStopCause,
    ) -> Result<ProcessMutationResult, ProcessExecutionError> {
        let Some(pid) = record.pid else {
            return Err(ProcessExecutionError::OperationFailed);
        };
        if !record
            .process_identity
            .as_deref()
            .is_some_and(|identity| platform::identity_matches(pid, identity))
        {
            let lost = self.mark_lost(&record).await?;
            return Ok(ProcessMutationResult {
                status: ProcessMutationStatus::AlreadyExited,
                view: Some(self.view(lost, WAIT_OUTPUT_CHARS)),
            });
        }
        let identity = record
            .process_identity
            .as_deref()
            .ok_or(ProcessExecutionError::OperationFailed)?;
        if platform::terminate_detached_tree(pid, identity)
            .await
            .is_err()
        {
            if !platform::identity_matches(pid, identity) {
                let lost = self.mark_lost(&record).await?;
                return Ok(ProcessMutationResult {
                    status: ProcessMutationStatus::AlreadyExited,
                    view: Some(self.view(lost, WAIT_OUTPUT_CHARS)),
                });
            }
            return Err(ProcessExecutionError::OperationFailed);
        }
        let killed = self
            .transition(
                owner,
                process_id,
                ProcessStatus::Running,
                cause.transition(),
            )
            .await?;
        Ok(ProcessMutationResult {
            status: ProcessMutationStatus::Killed,
            view: Some(self.view(killed, WAIT_OUTPUT_CHARS)),
        })
    }

    pub(crate) async fn write(
        &self,
        owner: ProcessOwner,
        process_id: &str,
        mut data: Vec<u8>,
        submit: bool,
    ) -> Result<ProcessMutationResult, ProcessExecutionError> {
        if submit {
            data.push(b'\n');
        }
        let record = match self.load(&owner, process_id).await {
            Ok(record) => record,
            Err(ProcessExecutionError::NotFound) => return Ok(not_found_mutation()),
            Err(error) => return Err(error),
        };
        if record.status.is_terminal() {
            return Ok(ProcessMutationResult {
                status: ProcessMutationStatus::AlreadyExited,
                view: Some(self.view(record, WAIT_OUTPUT_CHARS)),
            });
        }
        let entry = self
            .entry(process_id)
            .ok_or(ProcessExecutionError::StdinUnavailable)?;
        let (reply, response) = oneshot::channel();
        tokio::time::timeout(
            PROCESS_CONTROL_TIMEOUT,
            entry
                .supervisor
                .send(SupervisorCommand::Write { data, reply }),
        )
        .await
        .map_err(|_| ProcessExecutionError::OperationFailed)?
        .map_err(|_| ProcessExecutionError::OperationFailed)?;
        match tokio::time::timeout(PROCESS_CONTROL_TIMEOUT, response).await {
            Ok(Ok(Ok(()))) => Ok(ProcessMutationResult {
                status: if submit {
                    ProcessMutationStatus::Submitted
                } else {
                    ProcessMutationStatus::Written
                },
                view: Some(self.view(entry.status.borrow().clone(), WAIT_OUTPUT_CHARS)),
            }),
            Ok(Ok(Err(SupervisorError::StdinUnavailable))) => {
                Err(ProcessExecutionError::StdinUnavailable)
            }
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => Err(ProcessExecutionError::OperationFailed),
        }
    }

    pub(crate) async fn close(
        &self,
        owner: ProcessOwner,
        process_id: &str,
    ) -> Result<ProcessMutationResult, ProcessExecutionError> {
        let record = match self.load(&owner, process_id).await {
            Ok(record) => record,
            Err(ProcessExecutionError::NotFound) => return Ok(not_found_mutation()),
            Err(error) => return Err(error),
        };
        if record.status.is_terminal() {
            return Ok(ProcessMutationResult {
                status: ProcessMutationStatus::AlreadyExited,
                view: Some(self.view(record, WAIT_OUTPUT_CHARS)),
            });
        }
        let entry = self
            .entry(process_id)
            .ok_or(ProcessExecutionError::StdinUnavailable)?;
        let (reply, response) = oneshot::channel();
        tokio::time::timeout(
            PROCESS_CONTROL_TIMEOUT,
            entry.supervisor.send(SupervisorCommand::Close { reply }),
        )
        .await
        .map_err(|_| ProcessExecutionError::OperationFailed)?
        .map_err(|_| ProcessExecutionError::OperationFailed)?;
        match tokio::time::timeout(PROCESS_CONTROL_TIMEOUT, response).await {
            Ok(Ok(Ok(()))) => Ok(ProcessMutationResult {
                status: ProcessMutationStatus::Closed,
                view: Some(self.view(entry.status.borrow().clone(), WAIT_OUTPUT_CHARS)),
            }),
            Ok(Ok(Err(SupervisorError::StdinUnavailable))) => {
                Err(ProcessExecutionError::StdinUnavailable)
            }
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => Err(ProcessExecutionError::OperationFailed),
        }
    }

    async fn start_background(
        &self,
        context: ProcessExecutionContext,
        prepared: PreparedTerminalCommand,
        mut cancellation: watch::Receiver<bool>,
        run_deadline: Instant,
        async_delivery: Option<AsyncToolDeliveryRequest>,
    ) -> Result<TerminalExecutionResult, ProcessExecutionError> {
        // Serialize durable launch registration with shutdown so a process
        // cannot appear after the shutdown candidate snapshot.
        let _launch_gate = self.inner.launch_gate.lock().await;
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(ProcessExecutionError::OperationFailed);
        }
        let PreparedTerminalCommand {
            command_text,
            cwd,
            shell,
            redactor,
            control,
        } = prepared;
        check_control(&control)?;
        if *cancellation.borrow() {
            return Err(ProcessExecutionError::Cancelled);
        }
        let workspace_id = context
            .workspace_id
            .clone()
            .ok_or(ProcessExecutionError::WorkspaceRequired)?;
        let owner = context.owner();
        let process_id = format!("process_{}", Uuid::new_v4().simple());
        let command_sha256 = hex_digest(command_text.as_bytes());
        let create = CreateProcess {
            process_id: process_id.clone(),
            workspace_id,
            creator_run_id: context.creator_run_id.clone(),
            call_id: context.call_id.clone(),
            command_preview: format!("command sha256:{}", &command_sha256[..12]),
            command_sha256,
            detached: true,
            completion_notification_required: async_delivery.is_some(),
            async_delivery,
        };
        store_call({
            let sessions = self.inner.sessions.clone();
            let owner = owner.clone();
            let create = create.clone();
            move || sessions.reserve_process(&owner, &create, ACTIVE_PROCESS_LIMIT)
        })
        .await?;

        let (mut command, launch_frame) = match shell.guarded_command(&command_text, &cwd, false) {
            Ok(command) => command,
            Err(_) => {
                let _ = self
                    .transition(
                        owner.clone(),
                        &process_id,
                        ProcessStatus::Starting,
                        ProcessTransition::FailedStart {
                            completion_reason: "process command setup failed".to_owned(),
                            termination_source: "terminal".to_owned(),
                        },
                    )
                    .await;
                return Err(ProcessExecutionError::SpawnFailed);
            }
        };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                let _ = self
                    .transition(
                        owner.clone(),
                        &process_id,
                        ProcessStatus::Starting,
                        ProcessTransition::FailedStart {
                            completion_reason: "process spawn failed".to_owned(),
                            termination_source: "terminal".to_owned(),
                        },
                    )
                    .await;
                return Err(ProcessExecutionError::SpawnFailed);
            }
        };
        let Some(pid) = child.id() else {
            let _ = self
                .transition(
                    owner.clone(),
                    &process_id,
                    ProcessStatus::Starting,
                    ProcessTransition::FailedStart {
                        completion_reason: "spawned process had no process id".to_owned(),
                        termination_source: "terminal".to_owned(),
                    },
                )
                .await;
            return Err(ProcessExecutionError::SpawnFailed);
        };
        let lifetime = match platform::process_lifetime(pid) {
            Ok(lifetime) => lifetime,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let _ = self
                    .transition(
                        owner.clone(),
                        &process_id,
                        ProcessStatus::Starting,
                        ProcessTransition::FailedStart {
                            completion_reason: "process containment failed".to_owned(),
                            termination_source: "terminal".to_owned(),
                        },
                    )
                    .await;
                return Err(ProcessExecutionError::SpawnFailed);
            }
        };
        let Some(identity) = platform::process_identity(pid) else {
            let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
            let _ = self
                .transition(
                    owner.clone(),
                    &process_id,
                    ProcessStatus::Starting,
                    ProcessTransition::FailedStart {
                        completion_reason: "strong process identity was unavailable".to_owned(),
                        termination_source: "terminal".to_owned(),
                    },
                )
                .await;
            return Err(ProcessExecutionError::SpawnFailed);
        };
        let running = match self
            .transition(
                owner.clone(),
                &process_id,
                ProcessStatus::Starting,
                ProcessTransition::Running {
                    pid,
                    process_identity: Some(identity.clone()),
                },
            )
            .await
        {
            Ok(record) => record,
            Err(error) => {
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                return Err(error);
            }
        };

        let capture = ProcessOutputCapture::new(CaptureMode::Background);
        let drains = match take_output_drains(&mut child, capture.clone()) {
            Ok(drains) => drains,
            Err(error) => {
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                let _ = self
                    .transition(
                        owner.clone(),
                        &process_id,
                        ProcessStatus::Running,
                        ProcessTransition::Killed {
                            exit_code: None,
                            completion_reason: "process pipes were unavailable".to_owned(),
                            termination_source: "terminal".to_owned(),
                        },
                    )
                    .await;
                return Err(error);
            }
        };
        let Some(mut stdin) = child.stdin.take() else {
            let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
            await_drains(drains).await;
            let _ = self
                .transition(
                    owner,
                    &process_id,
                    ProcessStatus::Running,
                    ProcessTransition::Killed {
                        exit_code: None,
                        completion_reason: "process guardian stdin was unavailable".to_owned(),
                        termination_source: "terminal".to_owned(),
                    },
                )
                .await;
            return Err(ProcessExecutionError::SpawnFailed);
        };

        if *cancellation.borrow() || check_control(&control).is_err() {
            let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
            await_drains(drains).await;
            let _ = self
                .transition(
                    owner,
                    &process_id,
                    ProcessStatus::Running,
                    ProcessTransition::Killed {
                        exit_code: None,
                        completion_reason: "run cancelled before process result committed"
                            .to_owned(),
                        termination_source: "run_cancellation".to_owned(),
                    },
                )
                .await;
            return Err(ProcessExecutionError::Cancelled);
        }
        let launch_result = tokio::select! {
            biased;
            _ = cancellation.changed() => Err(ProcessExecutionError::Cancelled),
            _ = tokio::time::sleep_until(run_deadline.into()) => {
                Err(ProcessExecutionError::DeadlineExceeded)
            }
            result = write_guardian_frame(&mut stdin, &launch_frame) => {
                result.map_err(|_| ProcessExecutionError::SpawnFailed)
            }
        };
        if let Err(error) = launch_result {
            let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
            await_drains(drains).await;
            let (completion_reason, termination_source) = match &error {
                ProcessExecutionError::Cancelled => {
                    ("run cancelled during process launch", "run_cancellation")
                }
                ProcessExecutionError::DeadlineExceeded => {
                    ("run deadline elapsed during process launch", "run_deadline")
                }
                _ => ("process guardian launch failed", "terminal"),
            };
            let _ = self
                .transition(
                    owner,
                    &process_id,
                    ProcessStatus::Running,
                    ProcessTransition::Killed {
                        exit_code: None,
                        completion_reason: completion_reason.to_owned(),
                        termination_source: termination_source.to_owned(),
                    },
                )
                .await;
            return Err(error);
        }

        let (status_sender, status) = watch::channel(running);
        let (supervisor, receiver) = mpsc::channel(SUPERVISOR_CAPACITY);
        let entry = Arc::new(ManagedProcess {
            pid,
            identity,
            output: capture.clone(),
            redactor,
            status,
            supervisor,
        });
        self.inner
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(process_id.clone(), entry);
        tokio::spawn(supervise_background(
            self.inner.sessions.clone(),
            owner,
            process_id.clone(),
            pid,
            child,
            Some(stdin),
            drains,
            receiver,
            status_sender,
            lifetime,
            self.inner.entries.clone(),
            cancellation,
            run_deadline,
        ));

        Ok(TerminalExecutionResult::Background { process_id, pid })
    }

    async fn load(
        &self,
        owner: &ProcessOwner,
        process_id: &str,
    ) -> Result<ProcessRecord, ProcessExecutionError> {
        store_call({
            let sessions = self.inner.sessions.clone();
            let owner = owner.clone();
            let process_id = process_id.to_owned();
            move || sessions.get_process(&owner, &process_id)
        })
        .await
    }

    async fn transition(
        &self,
        owner: ProcessOwner,
        process_id: &str,
        expected: ProcessStatus,
        transition: ProcessTransition,
    ) -> Result<ProcessRecord, ProcessExecutionError> {
        store_call({
            let sessions = self.inner.sessions.clone();
            let process_id = process_id.to_owned();
            move || sessions.transition_process(&owner, &process_id, expected, &transition)
        })
        .await
    }

    async fn refresh_detached(
        &self,
        record: ProcessRecord,
    ) -> Result<ProcessRecord, ProcessExecutionError> {
        if record.status != ProcessStatus::Running || self.entry(&record.process_id).is_some() {
            return Ok(record);
        }
        let valid = record
            .pid
            .zip(record.process_identity.as_deref())
            .is_some_and(|(pid, identity)| platform::identity_matches(pid, identity));
        if valid {
            Ok(record)
        } else {
            self.mark_lost(&record).await
        }
    }

    async fn mark_lost(
        &self,
        record: &ProcessRecord,
    ) -> Result<ProcessRecord, ProcessExecutionError> {
        store_call({
            let sessions = self.inner.sessions.clone();
            let owner = ProcessOwner {
                profile_id: record.profile_id.clone(),
                session_id: record.session_id.clone(),
            };
            let process_id = record.process_id.clone();
            let expected = record.status;
            move || sessions.mark_process_lost(&owner, &process_id, expected)
        })
        .await
    }

    fn entry(&self, process_id: &str) -> Option<Arc<ManagedProcess>> {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(process_id)
            .cloned()
    }

    fn view(&self, record: ProcessRecord, maximum_chars: usize) -> ProcessView {
        let output = self.output_for(&record.process_id, maximum_chars);
        ProcessView { record, output }
    }

    fn output_for(&self, process_id: &str, maximum_chars: usize) -> String {
        let Some(entry) = self.entry(process_id) else {
            return String::new();
        };
        let output = entry.output.finish(entry.redactor.as_ref()).text;
        if maximum_chars == usize::MAX {
            output
        } else {
            tail_chars(&output, maximum_chars)
        }
    }

    fn reconcile_recovery_candidates(&self) {
        let Ok(candidates) = self.inner.sessions.list_recovery_candidates() else {
            return;
        };
        for record in candidates {
            let valid_detached = record.detached
                && record
                    .pid
                    .zip(record.process_identity.as_deref())
                    .is_some_and(|(pid, identity)| platform::identity_matches(pid, identity));
            if !valid_detached {
                let owner = ProcessOwner {
                    profile_id: record.profile_id,
                    session_id: record.session_id,
                };
                let _ = self.inner.sessions.mark_process_lost(
                    &owner,
                    &record.process_id,
                    record.status,
                );
            }
        }
    }

    async fn prune_finished(&self) {
        let cutoff_time = OffsetDateTime::now_utc() - FINISHED_PROCESS_TTL;
        let Ok(cutoff) = cutoff_time.format(&Rfc3339) else {
            return;
        };
        self.inner
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, entry| {
                let record = entry.status.borrow();
                !record.status.is_terminal()
                    || record.finished_at.as_deref().is_none_or(|finished_at| {
                        OffsetDateTime::parse(finished_at, &Rfc3339)
                            .map_or(true, |finished_at| finished_at > cutoff_time)
                    })
            });
        let sessions = self.inner.sessions.clone();
        let _ =
            tokio::task::spawn_blocking(move || sessions.prune_finished_processes(&cutoff)).await;
    }
}

async fn execute_foreground(
    manager: &ProcessManagerInner,
    prepared: PreparedTerminalCommand,
    requested_timeout: StdDuration,
    cancellation: &mut watch::Receiver<bool>,
    run_deadline: Instant,
) -> Result<TerminalExecutionResult, ProcessExecutionError> {
    let PreparedTerminalCommand {
        command_text,
        cwd,
        shell,
        redactor,
        control,
    } = prepared;
    check_control(&control)?;
    if *cancellation.borrow() {
        return Err(ProcessExecutionError::Cancelled);
    }
    let launch_gate = manager.launch_gate.lock().await;
    if manager.shutting_down.load(Ordering::Acquire) {
        return Err(ProcessExecutionError::OperationFailed);
    }
    check_control(&control)?;
    if *cancellation.borrow() {
        return Err(ProcessExecutionError::Cancelled);
    }
    let (mut command, launch_frame) = shell
        .guarded_command(&command_text, &cwd, true)
        .map_err(|_| ProcessExecutionError::SpawnFailed)?;
    let mut child = command
        .spawn()
        .map_err(|_| ProcessExecutionError::SpawnFailed)?;
    let pid = child.id().ok_or(ProcessExecutionError::SpawnFailed)?;
    let lifetime = match platform::process_lifetime(pid) {
        Ok(lifetime) => lifetime,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(ProcessExecutionError::SpawnFailed);
        }
    };
    let Some(identity) = platform::process_identity(pid) else {
        let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
        let _ = platform::finish_after_root_exit(pid, lifetime);
        return Err(ProcessExecutionError::SpawnFailed);
    };
    let mut process = ForegroundProcess::new(pid, identity, child, lifetime);
    let mut guardian_stdin = process
        .child
        .stdin
        .take()
        .ok_or(ProcessExecutionError::SpawnFailed)?;
    let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
    let drains = take_output_drains(&mut process.child, capture.clone())?;
    let close_frame = encode_process_guardian_stdin_close();
    let launch_result = tokio::select! {
        biased;
        _ = cancellation.changed() => Err(ProcessExecutionError::Cancelled),
        _ = tokio::time::sleep_until(run_deadline.into()) => {
            Err(ProcessExecutionError::DeadlineExceeded)
        }
        result = write_guardian_frame(&mut guardian_stdin, &launch_frame) => {
            result.map_err(|_| ProcessExecutionError::SpawnFailed)
        }
    };
    let launch_result = match launch_result {
        Ok(()) => tokio::select! {
            biased;
            _ = cancellation.changed() => Err(ProcessExecutionError::Cancelled),
            _ = tokio::time::sleep_until(run_deadline.into()) => {
                Err(ProcessExecutionError::DeadlineExceeded)
            }
            result = write_guardian_frame(&mut guardian_stdin, &close_frame) => {
                result.map_err(|_| ProcessExecutionError::SpawnFailed)
            }
        },
        error => error,
    };
    if let Err(error) = launch_result {
        let termination = process.terminate().await;
        let tree_cleanup = process.finish_after_root_exit();
        await_drains(drains).await;
        if let Err(cleanup_error) = termination {
            tracing::warn!(
                pid,
                original_error = ?error,
                error = ?cleanup_error,
                "failed to terminate process tree after foreground launch failure"
            );
        }
        if let Err(cleanup_error) = tree_cleanup {
            tracing::warn!(
                pid,
                original_error = ?error,
                error = ?cleanup_error,
                "failed to finish process tree cleanup after foreground launch failure"
            );
        }
        return Err(error);
    }
    drop(launch_gate);
    let command_deadline = Instant::now()
        .checked_add(requested_timeout)
        .unwrap_or(run_deadline);
    let effective_deadline = command_deadline.min(run_deadline);

    let mut termination = Ok(());
    let result = loop {
        tokio::select! {
            status = process.child.wait() => {
                let status = status.map_err(|_| ProcessExecutionError::OperationFailed)?;
                break Ok((exit_code(status), None));
            }
            changed = cancellation.changed() => {
                if changed.is_err() || *cancellation.borrow() {
                    termination = process.terminate().await;
                    break Err(ProcessExecutionError::Cancelled);
                }
            }
            _ = tokio::time::sleep_until(effective_deadline.into()) => {
                termination = process.terminate().await;
                if effective_deadline >= run_deadline && command_deadline >= run_deadline {
                    break Err(ProcessExecutionError::DeadlineExceeded);
                }
                break Ok((124, Some("Command timed out".to_owned())));
            }
        }
    };
    let tree_cleanup = process.finish_after_root_exit();
    drop(guardian_stdin);
    await_drains(drains).await;
    if let Err(cleanup_error) = &termination {
        tracing::error!(
            pid,
            error = ?cleanup_error,
            "failed to confirm foreground process tree termination"
        );
    }
    if let Err(cleanup_error) = &tree_cleanup {
        tracing::error!(
            pid,
            error = ?cleanup_error,
            "failed to finish foreground process tree cleanup"
        );
    }
    let result = foreground_result_after_cleanup(result, termination, tree_cleanup);
    let output = capture
        .finish_bounded(redactor.as_ref(), FOREGROUND_OUTPUT_LIMIT)
        .text;
    result.map(|(exit_code, error)| TerminalExecutionResult::Foreground {
        output,
        exit_code,
        error,
    })
}

fn foreground_result_after_cleanup<T>(
    result: Result<T, ProcessExecutionError>,
    termination: io::Result<()>,
    tree_cleanup: io::Result<()>,
) -> Result<T, ProcessExecutionError> {
    if termination.is_err() || tree_cleanup.is_err() {
        return Err(ProcessExecutionError::OperationFailed);
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn supervise_background(
    sessions: Arc<SessionService>,
    owner: ProcessOwner,
    process_id: String,
    pid: u32,
    mut child: Child,
    mut stdin: Option<ChildStdin>,
    drains: Vec<JoinHandle<io::Result<()>>>,
    mut commands: mpsc::Receiver<SupervisorCommand>,
    status_sender: watch::Sender<ProcessRecord>,
    lifetime: platform::ProcessLifetime,
    entries: Arc<Mutex<HashMap<String, Arc<ManagedProcess>>>>,
    mut launch_cancellation: watch::Receiver<bool>,
    run_deadline: Instant,
) {
    let mut drains = Some(drains);
    let mut lifetime = Some(lifetime);
    let (stdin_sender, stdin_task) = match stdin.take() {
        Some(stdin) => {
            let (sender, receiver) = mpsc::channel(STDIN_CAPACITY);
            (
                Some(sender),
                Some(tokio::spawn(supervise_stdin(stdin, receiver))),
            )
        }
        None => (None, None),
    };
    let mut retain_entry = true;
    let mut launch_committed = false;
    let mut stdin_closed = false;
    loop {
        tokio::select! {
            biased;
            changed = launch_cancellation.changed(), if !launch_committed => {
                if changed.is_err() || *launch_cancellation.borrow() {
                    let result = stop_background_process(
                        sessions.clone(),
                        owner.clone(),
                        process_id.clone(),
                        pid,
                        &mut child,
                        &mut lifetime,
                        &mut drains,
                        ProcessTransition::Killed {
                            exit_code: None,
                            completion_reason: "run cancelled before process launch committed".to_owned(),
                            termination_source: "run_cancellation".to_owned(),
                        },
                    ).await;
                    publish_background_result(&result, &status_sender, &mut retain_entry);
                    break;
                }
            }
            _ = tokio::time::sleep_until(run_deadline.into()), if !launch_committed => {
                let result = stop_background_process(
                    sessions.clone(),
                    owner.clone(),
                    process_id.clone(),
                    pid,
                    &mut child,
                    &mut lifetime,
                    &mut drains,
                    ProcessTransition::Killed {
                        exit_code: None,
                        completion_reason: "run deadline elapsed before process launch committed".to_owned(),
                        termination_source: "run_deadline".to_owned(),
                    },
                ).await;
                publish_background_result(&result, &status_sender, &mut retain_entry);
                break;
            }
            status = child.wait() => {
                let cleanup = platform::finish_after_root_exit(
                    pid,
                    lifetime.take().expect("process lifetime is present until completion"),
                );
                await_drains(drains.take().unwrap_or_default()).await;
                let transition = match (status, cleanup) {
                    (_, Err(_)) => ProcessTransition::Lost {
                        completion_reason: "process tree cleanup failed".to_owned(),
                        termination_source: "supervisor".to_owned(),
                    },
                    (Ok(status), Ok(())) => ProcessTransition::Exited {
                        exit_code: exit_code(status),
                        completion_reason: "process exited".to_owned(),
                        termination_source: "natural".to_owned(),
                    },
                    (Err(_), Ok(())) => ProcessTransition::Lost {
                        completion_reason: "process status became unavailable".to_owned(),
                        termination_source: "supervisor".to_owned(),
                    },
                };
                match persist_terminal_transition(
                    sessions.clone(),
                    owner.clone(),
                    process_id.clone(),
                    transition,
                ).await {
                    Ok(record) => {
                        status_sender.send_replace(record);
                    }
                    Err(()) => {
                        status_sender.send_replace(fallback_lost_record(&status_sender));
                        retain_entry = false;
                    }
                }
                break;
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    let result = stop_background_process(
                        sessions.clone(),
                        owner.clone(),
                        process_id.clone(),
                        pid,
                        &mut child,
                        &mut lifetime,
                        &mut drains,
                        ProcessTransition::Killed {
                            exit_code: None,
                            completion_reason: "process stopped with its supervisor".to_owned(),
                            termination_source: "backend_shutdown".to_owned(),
                        },
                    ).await;
                    publish_background_result(&result, &status_sender, &mut retain_entry);
                    break;
                };
                match command {
                    SupervisorCommand::CommitLaunch { reply } => {
                        if *launch_cancellation.borrow() || Instant::now() >= run_deadline {
                            let _ = reply.send(Err(SupervisorError::OperationFailed));
                            let result = stop_background_process(
                                sessions.clone(),
                                owner.clone(),
                                process_id.clone(),
                                pid,
                                &mut child,
                                &mut lifetime,
                                &mut drains,
                                ProcessTransition::Killed {
                                    exit_code: None,
                                    completion_reason: "run cancelled before process launch committed".to_owned(),
                                    termination_source: "run_cancellation".to_owned(),
                                },
                            ).await;
                            publish_background_result(&result, &status_sender, &mut retain_entry);
                            break;
                        }
                        launch_committed = true;
                        let _ = reply.send(Ok(()));
                    }
                    SupervisorCommand::Stop { cause, reply } => {
                        let result = stop_background_process(
                            sessions.clone(),
                            owner.clone(),
                            process_id.clone(),
                            pid,
                            &mut child,
                            &mut lifetime,
                            &mut drains,
                            cause.transition(),
                        ).await;
                        publish_background_result(&result, &status_sender, &mut retain_entry);
                        let _ = reply.send(result);
                        break;
                    }
                    SupervisorCommand::Write { data, reply } => {
                        if stdin_closed {
                            let _ = reply.send(Err(SupervisorError::StdinUnavailable));
                        } else {
                            forward_stdin_command(
                                stdin_sender.as_ref(),
                                StdinCommand::Write { data, reply },
                            );
                        }
                    }
                    SupervisorCommand::Close { reply } => {
                        if stdin_closed {
                            let _ = reply.send(Err(SupervisorError::StdinUnavailable));
                        } else {
                            stdin_closed = true;
                            forward_stdin_command(
                                stdin_sender.as_ref(),
                                StdinCommand::Close { reply },
                            );
                        }
                    }
                }
            }
        }
    }
    drop(stdin_sender);
    if let Some(task) = stdin_task {
        task.abort();
    }
    if !retain_entry {
        entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&process_id);
    }
}

async fn stop_managed_process(
    entry: Arc<ManagedProcess>,
    cause: ProcessStopCause,
) -> Result<ProcessRecord, SupervisorError> {
    let (reply, response) = oneshot::channel();
    tokio::time::timeout(
        PROCESS_CONTROL_TIMEOUT,
        entry
            .supervisor
            .send(SupervisorCommand::Stop { cause, reply }),
    )
    .await
    .map_err(|_| SupervisorError::OperationFailed)?
    .map_err(|_| SupervisorError::OperationFailed)?;
    tokio::time::timeout(PROCESS_CONTROL_TIMEOUT, response)
        .await
        .map_err(|_| SupervisorError::OperationFailed)?
        .map_err(|_| SupervisorError::OperationFailed)?
}

fn forward_stdin_command(sender: Option<&mpsc::Sender<StdinCommand>>, command: StdinCommand) {
    let Some(sender) = sender else {
        reject_stdin_command(command, SupervisorError::StdinUnavailable);
        return;
    };
    if let Err(error) = sender.try_send(command) {
        reject_stdin_command(error.into_inner(), SupervisorError::OperationFailed);
    }
}

fn reject_stdin_command(command: StdinCommand, error: SupervisorError) {
    match command {
        StdinCommand::Write { reply, .. } | StdinCommand::Close { reply } => {
            let _ = reply.send(Err(error));
        }
    }
}

async fn supervise_stdin(mut stdin: ChildStdin, mut commands: mpsc::Receiver<StdinCommand>) {
    let mut closed = false;
    while let Some(command) = commands.recv().await {
        match command {
            StdinCommand::Write { data, reply } => {
                if closed {
                    let _ = reply.send(Err(SupervisorError::StdinUnavailable));
                    continue;
                }
                let frame = match encode_process_guardian_stdin(&data) {
                    Ok(frame) => frame,
                    Err(_) => {
                        let _ = reply.send(Err(SupervisorError::OperationFailed));
                        continue;
                    }
                };
                let written = tokio::time::timeout(STDIN_WRITE_TIMEOUT, async {
                    stdin.write_all(&frame).await?;
                    stdin.flush().await
                })
                .await;
                let result = match written {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) | Err(_) => Err(SupervisorError::OperationFailed),
                };
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    break;
                }
            }
            StdinCommand::Close { reply } => {
                if closed {
                    let _ = reply.send(Err(SupervisorError::StdinUnavailable));
                    continue;
                }
                let frame = encode_process_guardian_stdin_close();
                let written = tokio::time::timeout(STDIN_WRITE_TIMEOUT, async {
                    stdin.write_all(&frame).await?;
                    stdin.flush().await
                })
                .await;
                match written {
                    Ok(Ok(())) => {
                        closed = true;
                        let _ = reply.send(Ok(()));
                    }
                    Ok(Err(_)) | Err(_) => {
                        let _ = reply.send(Err(SupervisorError::OperationFailed));
                        break;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stop_background_process(
    sessions: Arc<SessionService>,
    owner: ProcessOwner,
    process_id: String,
    pid: u32,
    child: &mut Child,
    lifetime: &mut Option<platform::ProcessLifetime>,
    drains: &mut Option<Vec<JoinHandle<io::Result<()>>>>,
    transition: ProcessTransition,
) -> Result<ProcessRecord, SupervisorError> {
    let Some(process_lifetime) = lifetime.as_ref() else {
        return Err(SupervisorError::OperationFailed);
    };
    let killed = platform::terminate_tree(pid, child, process_lifetime).await;
    let cleanup = lifetime
        .take()
        .ok_or(SupervisorError::OperationFailed)
        .and_then(|lifetime| {
            platform::finish_after_root_exit(pid, lifetime)
                .map_err(|_| SupervisorError::OperationFailed)
        });
    await_drains(drains.take().unwrap_or_default()).await;
    if killed.is_err() || cleanup.is_err() {
        return Err(SupervisorError::OperationFailed);
    }
    persist_terminal_transition(sessions, owner, process_id, transition)
        .await
        .map_err(|()| SupervisorError::StorageUnavailable)
}

fn publish_background_result(
    result: &Result<ProcessRecord, SupervisorError>,
    status_sender: &watch::Sender<ProcessRecord>,
    retain_entry: &mut bool,
) {
    match result {
        Ok(record) => {
            status_sender.send_replace(record.clone());
        }
        Err(_) => {
            status_sender.send_replace(fallback_lost_record(status_sender));
            *retain_entry = false;
        }
    }
}

fn take_output_drains(
    child: &mut Child,
    capture: ProcessOutputCapture,
) -> Result<Vec<JoinHandle<io::Result<()>>>, ProcessExecutionError> {
    let stdout = child
        .stdout
        .take()
        .ok_or(ProcessExecutionError::SpawnFailed)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(ProcessExecutionError::SpawnFailed)?;
    Ok(vec![
        tokio::spawn(drain_stream(stdout, capture.clone(), ProcessStream::Stdout)),
        tokio::spawn(drain_stream(stderr, capture, ProcessStream::Stderr)),
    ])
}

async fn write_guardian_frame(stdin: &mut ChildStdin, frame: &[u8]) -> io::Result<()> {
    tokio::time::timeout(STDIN_WRITE_TIMEOUT, async {
        stdin.write_all(frame).await?;
        stdin.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "guardian launch timed out"))?
}

async fn drain_stream(
    mut reader: impl AsyncRead + Unpin,
    capture: ProcessOutputCapture,
    stream: ProcessStream,
) -> io::Result<()> {
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        capture.append(stream, &buffer[..read]);
    }
}

async fn await_drains(drains: Vec<JoinHandle<io::Result<()>>>) {
    let mut drains = drains;
    let joined = tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, async {
        for drain in &mut drains {
            let _ = drain.await;
        }
    })
    .await;
    if joined.is_err() {
        for drain in drains {
            drain.abort();
        }
    }
}

async fn persist_terminal_transition(
    sessions: Arc<SessionService>,
    owner: ProcessOwner,
    process_id: String,
    transition: ProcessTransition,
) -> Result<ProcessRecord, ()> {
    for attempt in 0..3 {
        match transition_store(
            sessions.clone(),
            owner.clone(),
            process_id.clone(),
            ProcessStatus::Running,
            transition.clone(),
        )
        .await
        {
            Ok(record) => return Ok(record),
            Err(ProcessExecutionError::StorageUnavailable) if attempt < 2 => {
                tokio::time::sleep(StdDuration::from_millis(50 * (attempt + 1))).await;
            }
            Err(_) => break,
        }
    }

    let latest = store_call({
        let sessions = sessions.clone();
        let owner = owner.clone();
        let process_id = process_id.clone();
        move || sessions.get_process(&owner, &process_id)
    })
    .await;
    match latest {
        Ok(record) if record.status.is_terminal() => Ok(record),
        _ => Err(()),
    }
}

fn fallback_lost_record(status_sender: &watch::Sender<ProcessRecord>) -> ProcessRecord {
    let mut record = status_sender.borrow().clone();
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| record.updated_at.clone());
    record.status = ProcessStatus::Lost;
    record.updated_at = timestamp.clone();
    record.finished_at = Some(timestamp);
    record.exit_code = None;
    record.completion_reason =
        Some("process exited but its terminal state could not be stored".to_owned());
    record.termination_source = Some("storage_failure".to_owned());
    record
}

async fn transition_store(
    sessions: Arc<SessionService>,
    owner: ProcessOwner,
    process_id: String,
    expected: ProcessStatus,
    transition: ProcessTransition,
) -> Result<ProcessRecord, ProcessExecutionError> {
    store_call(move || sessions.transition_process(&owner, &process_id, expected, &transition))
        .await
}

async fn store_call<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ProcessStoreError> + Send + 'static,
) -> Result<T, ProcessExecutionError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ProcessExecutionError::StorageUnavailable)?
        .map_err(map_store_error)
}

fn map_store_error(error: ProcessStoreError) -> ProcessExecutionError {
    match error {
        ProcessStoreError::NotFound => ProcessExecutionError::NotFound,
        ProcessStoreError::ProcessLimitReached { .. } => ProcessExecutionError::ProcessLimitReached,
        ProcessStoreError::InvalidRequest | ProcessStoreError::DataInvalid => {
            ProcessExecutionError::OperationFailed
        }
        ProcessStoreError::TransitionConflict { .. } => ProcessExecutionError::OperationFailed,
        ProcessStoreError::StorageBusy | ProcessStoreError::StorageUnavailable => {
            ProcessExecutionError::StorageUnavailable
        }
    }
}

fn check_control(control: &ToolExecutionControl) -> Result<(), ProcessExecutionError> {
    control.check().map_err(|error| match error {
        ToolExecutionControlError::Cancelled => ProcessExecutionError::Cancelled,
        ToolExecutionControlError::DeadlineExceeded => ProcessExecutionError::DeadlineExceeded,
    })
}

fn not_found_mutation() -> ProcessMutationResult {
    ProcessMutationResult {
        status: ProcessMutationStatus::NotFound,
        view: None,
    }
}

fn hex_digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tail_chars(value: &str, maximum: usize) -> String {
    let total = value.chars().count();
    value.chars().skip(total.saturating_sub(maximum)).collect()
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(target_os = "windows")]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

struct SecretMasker {
    exact: Vec<SecretString>,
    token_pattern: Regex,
    bearer_pattern: Regex,
}

impl SecretMasker {
    fn new(exact: Vec<SecretString>) -> Self {
        Self {
            exact,
            token_pattern: Regex::new(
                r"(?i)\b(?:sk|ghp|github_pat|xox[baprs]|AIza)[-_A-Za-z0-9]{12,}\b",
            )
            .expect("the built-in token redaction regex is valid"),
            bearer_pattern: Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{12,}")
                .expect("the built-in bearer redaction regex is valid"),
        }
    }
}

impl OutputRedactor for SecretMasker {
    fn redact(&self, value: &str) -> String {
        let mut redacted = value.to_owned();
        for secret in &self.exact {
            let secret = secret.expose_secret();
            if secret.len() >= 4 {
                redacted = redacted.replace(secret, "[REDACTED]");
            }
        }
        let redacted = self
            .token_pattern
            .replace_all(&redacted, "[REDACTED]")
            .into_owned();
        self.bearer_pattern
            .replace_all(&redacted, "$1[REDACTED]")
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::OnceLock, time::Instant};

    use crate::{
        runs::{ChatInput, CreateRun},
        sessions::{CreateSession, ProviderTurnFinish, ProviderTurnPlan, RawToolCallPlan, Usage},
    };

    use super::*;

    const TEST_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TERMINAL_FIXTURE_READY_TIMEOUT: StdDuration = StdDuration::from_secs(30);

    // These tests launch and kill real shell/guardian process trees. Keep the
    // short lifecycle assertions isolated from another test's concurrent Job
    // teardown, then use a per-command marker to establish target readiness.
    static TERMINAL_FIXTURE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    async fn lock_terminal_fixture() -> tokio::sync::MutexGuard<'static, ()> {
        TERMINAL_FIXTURE_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    async fn wait_for_fixture_marker<T>(
        marker: &Path,
        task: &JoinHandle<T>,
    ) -> Result<(), &'static str> {
        tokio::time::timeout(TERMINAL_FIXTURE_READY_TIMEOUT, async {
            loop {
                match tokio::fs::try_exists(marker).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(_) => return Err("the command readiness marker could not be inspected"),
                }
                if task.is_finished() {
                    return Err("the command completed before publishing its readiness marker");
                }
                tokio::time::sleep(StdDuration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "the command did not publish its readiness marker")?
    }

    async fn wait_for_fixture_pid<T>(
        pid_file: &Path,
        task: &JoinHandle<T>,
    ) -> Result<u32, &'static str> {
        tokio::time::timeout(TERMINAL_FIXTURE_READY_TIMEOUT, async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(pid_file).await
                    && let Ok(pid) = value.trim().parse::<u32>()
                {
                    return Ok(pid);
                }
                if task.is_finished() {
                    return Err("the command completed before publishing its pid");
                }
                tokio::time::sleep(StdDuration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "the command did not publish its pid")?
    }

    #[cfg(target_os = "windows")]
    fn foreground_pid_command(pid_file: &Path) -> String {
        let path = pid_file.to_string_lossy().replace('\\', "/");
        assert!(!path.contains('"'), "temporary path must be shell-quotable");
        format!(
            "powershell.exe -NoProfile -NonInteractive -Command \
             '[IO.File]::WriteAllText(\"{path}\", [string]$PID); Start-Sleep -Seconds 30'"
        )
    }

    #[cfg(not(target_os = "windows"))]
    fn foreground_pid_command(pid_file: &Path) -> String {
        let path = pid_file.to_string_lossy();
        assert!(
            !path.contains('\''),
            "temporary path must be shell-quotable"
        );
        format!("printf '%s' \"$$\" > '{path}'; sleep 30")
    }

    async fn wait_for_process_exit(pid: u32, identity: &str) -> Result<(), &'static str> {
        tokio::time::timeout(TERMINAL_FIXTURE_READY_TIMEOUT, async {
            while platform::identity_matches(pid, identity) {
                tokio::time::sleep(StdDuration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "the foreground process survived its execution future")
    }

    #[test]
    fn secret_masker_removes_exact_and_common_tokens() {
        let masker = SecretMasker::new(vec![SecretString::from("exact-secret".to_owned())]);
        let redacted =
            masker.redact("exact-secret Bearer abcdefghijklmnop sk-abcdefghijklmnopqrstuvwxyz");
        assert!(!redacted.contains("exact-secret"));
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(!redacted.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn tail_chars_is_unicode_safe() {
        assert_eq!(tail_chars("a界bc", 2), "bc");
        assert_eq!(tail_chars("a界bc", 3), "界bc");
    }

    #[test]
    fn foreground_cleanup_failure_overrides_original_outcome() {
        assert_eq!(
            foreground_result_after_cleanup::<()>(
                Err(ProcessExecutionError::Cancelled),
                Err(io::Error::other("injected termination failure")),
                Ok(()),
            ),
            Err(ProcessExecutionError::OperationFailed)
        );
        assert_eq!(
            foreground_result_after_cleanup::<()>(
                Err(ProcessExecutionError::DeadlineExceeded),
                Ok(()),
                Err(io::Error::other("injected final cleanup failure")),
            ),
            Err(ProcessExecutionError::OperationFailed)
        );
    }

    #[tokio::test]
    async fn foreground_runs_without_blocking_and_redacts_output() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(40);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let result = manager
            .execute_terminal(
                test_context(workspace.path()),
                TerminalExecutionRequest {
                    command: "printf 'stdout-ok\\n'; printf 'exact-secret\\n' >&2; exit 7"
                        .to_owned(),
                    background: false,
                    timeout: StdDuration::from_secs(5),
                    workdir: None,
                },
                vec![SecretString::from("exact-secret".to_owned())],
                control,
                cancellation,
                deadline,
            )
            .await
            .unwrap();
        let TerminalExecutionResult::Foreground {
            output,
            exit_code,
            error,
        } = result
        else {
            panic!("foreground execution must return a foreground result");
        };
        assert_eq!(exit_code, 7);
        assert_eq!(error, None);
        assert!(output.contains("stdout-ok"));
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("exact-secret"));
    }

    #[tokio::test]
    async fn foreground_timeout_is_a_valid_bounded_result() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(45);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let marker = workspace.path().join("foreground-timeout.ready");
        let task = tokio::spawn({
            let manager = manager.clone();
            let context = test_context(workspace.path());
            async move {
                manager
                    .execute_terminal(
                        context,
                        TerminalExecutionRequest {
                            command:
                                "printf 'before-timeout\\n'; : > foreground-timeout.ready; sleep 30"
                                    .to_owned(),
                            background: false,
                            timeout: StdDuration::from_secs(5),
                            workdir: None,
                        },
                        Vec::new(),
                        control,
                        cancellation,
                        deadline,
                    )
                    .await
            }
        });
        if let Err(reason) = wait_for_fixture_marker(&marker, &task).await {
            if task.is_finished() {
                let detail = match task.await {
                    Ok(Ok(_)) => "terminal task returned before writing the marker".to_owned(),
                    Ok(Err(error)) => format!("terminal task failed with {error:?}"),
                    Err(error) => format!("terminal task join failed with {error}"),
                };
                panic!("{reason}: {detail}");
            }
            task.abort();
            let _ = task.await;
            panic!("{reason}");
        }
        let result = task.await.unwrap().unwrap();
        let TerminalExecutionResult::Foreground {
            output,
            exit_code,
            error,
        } = result
        else {
            panic!("foreground execution must return a foreground result");
        };
        assert_eq!(exit_code, 124);
        assert_eq!(error.as_deref(), Some("Command timed out"));
        assert!(output.contains("before-timeout"));
    }

    #[tokio::test]
    async fn foreground_root_exit_cannot_hang_on_descendant_owned_pipes() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(50);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let marker = workspace.path().join("root-exit.ready");
        let mut task = tokio::spawn({
            let manager = manager.clone();
            let context = test_context(workspace.path());
            async move {
                manager
                    .execute_terminal(
                        context,
                        TerminalExecutionRequest {
                            command: "(sleep 30) & printf 'root-exited\\n'; : > root-exit.ready"
                                .to_owned(),
                            background: false,
                            timeout: StdDuration::from_secs(15),
                            workdir: None,
                        },
                        Vec::new(),
                        control,
                        cancellation,
                        deadline,
                    )
                    .await
            }
        });
        if let Err(reason) = wait_for_fixture_marker(&marker, &task).await {
            task.abort();
            let _ = task.await;
            panic!("{reason}");
        }
        let result = tokio::time::timeout(StdDuration::from_secs(5), &mut task)
            .await
            .expect("descendant-owned pipes must not outlive the command deadline")
            .unwrap()
            .unwrap();
        let TerminalExecutionResult::Foreground {
            output, exit_code, ..
        } = result
        else {
            panic!("foreground execution must return a foreground result");
        };
        assert_eq!(exit_code, 0);
        assert!(output.contains("root-exited"));
    }

    #[tokio::test]
    async fn foreground_cancellation_kills_the_spawned_process_tree() {
        let _fixture = lock_terminal_fixture().await;
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(50);
        let control = ToolExecutionControl::new(deadline);
        let task_control = control.clone();
        let (cancel_sender, cancellation) = watch::channel(false);
        let pid_file = workspace.path().join("foreground.pid");
        let command = foreground_pid_command(&pid_file);
        let mut task = tokio::spawn({
            let manager = manager.clone();
            let context = test_context(workspace.path());
            async move {
                manager
                    .execute_terminal(
                        context,
                        TerminalExecutionRequest {
                            command,
                            background: false,
                            timeout: StdDuration::from_secs(15),
                            workdir: None,
                        },
                        Vec::new(),
                        task_control,
                        cancellation,
                        deadline,
                    )
                    .await
            }
        });
        let pid = match wait_for_fixture_pid(&pid_file, &task).await {
            Ok(pid) => pid,
            Err(reason) => {
                control.cancel();
                let _ = cancel_sender.send(true);
                let _ = tokio::time::timeout(StdDuration::from_secs(5), &mut task).await;
                panic!("{reason}");
            }
        };
        let identity = platform::process_identity(pid).expect("foreground identity must exist");
        control.cancel();
        cancel_sender.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(StdDuration::from_secs(15), task)
                .await
                .expect("cancelled command should stop")
                .unwrap(),
            Err(ProcessExecutionError::Cancelled)
        );
        assert!(!platform::identity_matches(pid, &identity));
    }

    #[tokio::test]
    async fn shutdown_rejects_a_new_foreground_launch() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let manager = ProcessManager::new(sessions);
        manager
            .shutdown_all_until(Instant::now() + StdDuration::from_secs(30))
            .await;

        let deadline = Instant::now() + StdDuration::from_secs(30);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let marker = workspace.path().join("foreground-after-shutdown.ready");
        let result = manager
            .execute_terminal(
                test_context(workspace.path()),
                TerminalExecutionRequest {
                    command: ": > foreground-after-shutdown.ready".to_owned(),
                    background: false,
                    timeout: StdDuration::from_secs(5),
                    workdir: None,
                },
                Vec::new(),
                control,
                cancellation,
                deadline,
            )
            .await;

        assert_eq!(result, Err(ProcessExecutionError::OperationFailed));
        assert!(!tokio::fs::try_exists(marker).await.unwrap());
    }

    #[tokio::test]
    async fn aborting_a_foreground_execution_reclaims_the_exact_process_tree() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(50);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let pid_file = workspace.path().join("foreground-abort.pid");
        let command = foreground_pid_command(&pid_file);
        let task = tokio::spawn({
            let manager = manager.clone();
            let context = test_context(workspace.path());
            async move {
                manager
                    .execute_terminal(
                        context,
                        TerminalExecutionRequest {
                            command,
                            background: false,
                            timeout: StdDuration::from_secs(30),
                            workdir: None,
                        },
                        Vec::new(),
                        control,
                        cancellation,
                        deadline,
                    )
                    .await
            }
        });
        let pid = match wait_for_fixture_pid(&pid_file, &task).await {
            Ok(pid) => pid,
            Err(reason) => {
                task.abort();
                let _ = task.await;
                panic!("{reason}");
            }
        };
        let identity = platform::process_identity(pid).expect("foreground identity must exist");

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        wait_for_process_exit(pid, &identity).await.unwrap();
    }

    #[tokio::test]
    async fn background_submit_and_wait_share_one_supervised_process() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let session = sessions
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    persona_id: None,
                    title: Some("background process".to_owned()),
                },
                "background-session",
            )
            .unwrap();
        let registered = sessions
            .register_workspace(
                "default",
                workspace.path().to_str().unwrap(),
                "background-workspace",
            )
            .unwrap();
        let accepted = sessions
            .create_run(
                &session.value.id,
                &CreateRun {
                    persona_id: None,
                    client_request_id: "background-run".to_owned(),
                    message: ChatInput {
                        text: "run background process".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: Some(registered.id.clone()),
                },
                "background-run",
                "test-model",
            )
            .unwrap();
        let run = accepted.run;
        let message_id = "message_background";
        let call_id = "call_background";
        sessions
            .begin_assistant_message(&run.id, message_id)
            .unwrap();
        sessions
            .record_provider_turn(
                &run.id,
                &ProviderTurnPlan {
                    turn_index: 1,
                    assistant_message_id: message_id.to_owned(),
                    content: None,
                    reasoning: None,
                    finish: ProviderTurnFinish::ToolCalls,
                    usage: Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        cost: None,
                    },
                    tool_calls: vec![RawToolCallPlan {
                        call_id: call_id.to_owned(),
                        tool_name: "terminal".to_owned(),
                        arguments_json: "{}".to_owned(),
                    }],
                },
            )
            .unwrap();
        sessions
            .start_tool_invocation_with_event(
                &run.id,
                call_id,
                "terminal",
                "Background terminal command",
            )
            .unwrap();

        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(40);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let context = ProcessExecutionContext {
            profile_id: "default".to_owned(),
            session_id: run.session_id.clone(),
            workspace_id: Some(registered.id),
            workspace_root: Some(workspace.path().to_owned()),
            creator_run_id: run.id,
            call_id: call_id.to_owned(),
        };
        let started = manager
            .execute_terminal(
                context,
                TerminalExecutionRequest {
                    command: "while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; [ \"$line\" = stop ] && exit 0; done".to_owned(),
                    background: true,
                    timeout: StdDuration::from_secs(5),
                    workdir: None,
                },
                Vec::new(),
                control.clone(),
                cancellation.clone(),
                deadline,
            )
            .await
            .unwrap();
        let TerminalExecutionResult::Background { process_id, .. } = started else {
            panic!("background execution must return a process id");
        };
        let owner = ProcessOwner {
            profile_id: "default".to_owned(),
            session_id: run.session_id,
        };
        let submitted = manager
            .write(owner.clone(), &process_id, b"stop".to_vec(), true)
            .await
            .unwrap();
        assert_eq!(submitted.status, ProcessMutationStatus::Submitted);

        let waited = manager
            .wait(
                owner,
                &process_id,
                Some(StdDuration::from_secs(5)),
                control,
                cancellation,
                deadline,
            )
            .await
            .unwrap();
        assert_eq!(waited.status, ProcessWaitStatus::Exited);
        let view = waited.view.unwrap();
        assert_eq!(view.record.status, ProcessStatus::Exited);
        assert_eq!(view.record.exit_code, Some(0));
        assert!(view.output.contains("got:stop"));
    }

    #[tokio::test]
    async fn background_stdin_backpressure_cannot_block_tree_kill() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let (sessions, context, owner) = background_binding(&home, &workspace, "backpressure");
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(45);
        let control = ToolExecutionControl::new(deadline);
        let (_cancel_sender, cancellation) = watch::channel(false);
        let started = manager
            .execute_terminal(
                context,
                TerminalExecutionRequest {
                    command: "sleep 30".to_owned(),
                    background: true,
                    timeout: StdDuration::from_secs(5),
                    workdir: None,
                },
                Vec::new(),
                control,
                cancellation,
                deadline,
            )
            .await
            .unwrap();
        let TerminalExecutionResult::Background { process_id, pid } = started else {
            panic!("background execution must return a process id");
        };
        let identity = platform::process_identity(pid).expect("background identity must exist");

        let mut writers = Vec::new();
        for _ in 0..6 {
            writers.push(tokio::spawn({
                let manager = manager.clone();
                let owner = owner.clone();
                let process_id = process_id.clone();
                async move {
                    manager
                        .write(owner, &process_id, vec![b'x'; 32 * 1024], false)
                        .await
                }
            }));
        }
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        assert!(
            writers.iter().any(|writer| !writer.is_finished()),
            "the fixture must create real guardian stdin backpressure"
        );
        let killed =
            tokio::time::timeout(StdDuration::from_secs(5), manager.kill(owner, &process_id))
                .await
                .expect("kill must bypass a blocked stdin writer")
                .unwrap();
        assert_eq!(killed.status, ProcessMutationStatus::Killed);
        let mut released_with_failure = false;
        for writer in writers {
            let write_result = tokio::time::timeout(StdDuration::from_secs(5), writer)
                .await
                .expect("blocked stdin writers must be released")
                .unwrap();
            released_with_failure |= matches!(
                write_result,
                Err(ProcessExecutionError::OperationFailed)
                    | Err(ProcessExecutionError::StdinUnavailable)
            );
        }
        assert!(released_with_failure);
        assert!(!platform::identity_matches(pid, &identity));
    }

    #[tokio::test]
    async fn background_cancel_before_durable_commit_rolls_back_launch() {
        let _fixture = lock_terminal_fixture().await;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let (sessions, context, owner) = background_binding(&home, &workspace, "launch-lease");
        let manager = ProcessManager::new(sessions);
        let deadline = Instant::now() + StdDuration::from_secs(45);
        let control = ToolExecutionControl::new(deadline);
        let (cancel_sender, cancellation) = watch::channel(false);
        let started = manager
            .execute_terminal(
                context,
                TerminalExecutionRequest {
                    command: "sleep 30".to_owned(),
                    background: true,
                    timeout: StdDuration::from_secs(5),
                    workdir: None,
                },
                Vec::new(),
                control,
                cancellation,
                deadline,
            )
            .await
            .unwrap();
        let TerminalExecutionResult::Background { process_id, pid } = started else {
            panic!("background execution must return a process id");
        };
        let identity = platform::process_identity(pid).expect("background identity must exist");

        cancel_sender.send(true).unwrap();
        assert!(
            manager
                .commit_launch(owner.clone(), &process_id)
                .await
                .is_err()
        );
        let wait_control = ToolExecutionControl::new(deadline);
        let (_wait_sender, wait_cancellation) = watch::channel(false);
        let waited = manager
            .wait(
                owner,
                &process_id,
                Some(StdDuration::from_secs(5)),
                wait_control,
                wait_cancellation,
                deadline,
            )
            .await
            .unwrap();
        assert_eq!(waited.status, ProcessWaitStatus::Exited);
        let record = waited.view.unwrap().record;
        assert_eq!(record.status, ProcessStatus::Killed);
        assert_eq!(
            record.termination_source.as_deref(),
            Some("run_cancellation")
        );
        assert!(!platform::identity_matches(pid, &identity));
    }

    fn background_binding(
        home: &tempfile::TempDir,
        workspace: &tempfile::TempDir,
        key: &str,
    ) -> (Arc<SessionService>, ProcessExecutionContext, ProcessOwner) {
        let sessions = Arc::new(SessionService::new(home.path(), TEST_TOKEN));
        let session = sessions
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    persona_id: None,
                    title: Some(format!("background {key}")),
                },
                &format!("background-session-{key}"),
            )
            .unwrap();
        let registered = sessions
            .register_workspace(
                "default",
                workspace.path().to_str().unwrap(),
                &format!("background-workspace-{key}"),
            )
            .unwrap();
        let accepted = sessions
            .create_run(
                &session.value.id,
                &CreateRun {
                    persona_id: None,
                    client_request_id: format!("background-client-{key}"),
                    message: ChatInput {
                        text: "run background process".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: Some(registered.id.clone()),
                },
                &format!("background-run-key-{key}"),
                "test-model",
            )
            .unwrap();
        let run = accepted.run;
        let message_id = format!("message_background_{key}");
        let call_id = format!("call_background_{key}");
        sessions
            .begin_assistant_message(&run.id, &message_id)
            .unwrap();
        sessions
            .record_provider_turn(
                &run.id,
                &ProviderTurnPlan {
                    turn_index: 1,
                    assistant_message_id: message_id,
                    content: None,
                    reasoning: None,
                    finish: ProviderTurnFinish::ToolCalls,
                    usage: Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        cost: None,
                    },
                    tool_calls: vec![RawToolCallPlan {
                        call_id: call_id.clone(),
                        tool_name: "terminal".to_owned(),
                        arguments_json: "{}".to_owned(),
                    }],
                },
            )
            .unwrap();
        sessions
            .start_tool_invocation_with_event(
                &run.id,
                &call_id,
                "terminal",
                "Background terminal command",
            )
            .unwrap();
        let owner = ProcessOwner {
            profile_id: "default".to_owned(),
            session_id: run.session_id.clone(),
        };
        let context = ProcessExecutionContext {
            profile_id: "default".to_owned(),
            session_id: run.session_id,
            workspace_id: Some(registered.id),
            workspace_root: Some(workspace.path().to_owned()),
            creator_run_id: run.id,
            call_id,
        };
        (sessions, context, owner)
    }

    fn test_context(workspace: &Path) -> ProcessExecutionContext {
        ProcessExecutionContext {
            profile_id: "default".to_owned(),
            session_id: "session_test".to_owned(),
            workspace_id: Some("workspace_test".to_owned()),
            workspace_root: Some(workspace.to_owned()),
            creator_run_id: "run_test".to_owned(),
            call_id: "call_test".to_owned(),
        }
    }
}
