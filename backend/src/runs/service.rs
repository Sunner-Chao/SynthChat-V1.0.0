use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use secrecy::SecretString;
use sha2::{Digest, Sha256};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{broadcast, mpsc, watch},
    time::Instant,
};
use url::Url;
use uuid::Uuid;

use crate::{
    browser::BrowserManager,
    mcp::{McpRuntimeError, McpService, McpToolBinding},
    memory::{MemoryError, MemoryService},
    processes::{ProcessExecutionContext, ProcessManager},
    profiles::{ProfileError, ProfileService},
    providers::{
        OpenAiCompatibleProvider, ProviderError, ProviderEvent, ProviderFinish, ProviderMessage,
        ProviderRequest, ProviderToolDefinition, ProviderTransport, ProviderTurn, ProviderUsage,
    },
    sessions::{
        ClarificationContinuationBinding, ClarificationError, ClarificationRequest,
        ClarificationResolvedBy, ClarificationState, CompleteRunPlan, ProviderContextMessage,
        ProviderTurnFinish, ProviderTurnPlan, RawToolCallPlan, RuntimeLease, SessionError,
        SessionService, ToolApprovalDecision, ToolApprovalError, ToolApprovalExecutionBinding,
        ToolApprovalRequest, ToolApprovalResolvedBy, ToolApprovalState, ToolCall, ToolCallStatus,
        Usage,
        process_store::ProcessOwner,
        process_store::{
            AsyncToolDeliveryDisposition, AsyncToolDeliveryKind, AsyncToolDeliveryTrigger,
            PendingAsyncToolDelivery,
        },
    },
    skills::SkillService,
    tools::{
        PreparedClarification, PreparedToolCall, ToolExecutionContext, ToolExecutionControl,
        ToolExecutionError, ToolRegistry, ToolRisk,
    },
    web::WebService,
};

use super::{
    ActionAccepted, ActiveRunList, ApprovalChoice, ApprovalDecision, CancelDisposition,
    ClarificationAnswer, CreateRun, QueuedRunClaim, Run, RunAccepted, RunError, RunEventBatch,
    RunModelConfig, RunProblem, RunStatus,
    task_registry::{
        AdmissionGuard, RegistryError, ShutdownMode, StopControl, StopReason, TrackedRun,
        TrackedTaskRegistry,
    },
};

const EVENT_NOTIFICATION_CAPACITY: usize = 128;
const PROVIDER_EVENT_CAPACITY: usize = 64;
const MAX_PROVIDER_TURNS_PER_RUN: u32 = 8;
const MAX_TOOL_CALLS_PER_RUN: usize = 32;
const MAX_RUN_DURATION: Duration = Duration::from_secs(10 * 60);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RUNTIME_LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(5);
const ASYNC_TOOL_DELIVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TASK_GRACE: Duration = Duration::from_millis(750);
const OPENAI_COMPATIBLE_PROVIDERS: &[&str] = &[
    "openrouter",
    "custom",
    "openai-api",
    "lmstudio",
    "zai",
    "kimi-coding",
    "kimi-coding-cn",
    "stepfun",
    "arcee",
    "gmi",
    "alibaba",
    "alibaba-coding-plan",
    "deepseek",
    "xai",
    "nvidia",
    "opencode-zen",
    "opencode-go",
    "kilocode",
    "huggingface",
    "xiaomi",
    "tencent-tokenhub",
    "ollama-cloud",
    "deepinfra",
    "fireworks",
    "novita",
    "upstage",
];

#[derive(Clone)]
pub struct RunService {
    inner: Arc<RunServiceInner>,
}

struct RunServiceInner {
    profiles: Arc<ProfileService>,
    sessions: Arc<SessionService>,
    skills: Arc<SkillService>,
    memory: Arc<MemoryService>,
    web: Arc<WebService>,
    browser: BrowserManager,
    mcp: Arc<McpService>,
    provider: Arc<dyn ProviderTransport>,
    tools: ToolRegistry,
    processes: ProcessManager,
    notifications: Mutex<HashMap<String, broadcast::Sender<()>>>,
    tasks: Arc<TrackedTaskRegistry>,
    runtime_lease: Mutex<Option<RuntimeLease>>,
    runtime_lease_valid: AtomicBool,
    shutting_down: AtomicBool,
}

impl Drop for RunServiceInner {
    fn drop(&mut self) {
        if let Some(lease) = self
            .runtime_lease
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            self.sessions.release_runtime_lease(&lease);
        }
    }
}

struct PreparedModel {
    profile_id: String,
    provider_id: String,
    model: String,
    base_url: Url,
    url: Url,
    secret: Option<SecretString>,
    reasoning_effort: Option<String>,
    model_label: String,
    tools: Vec<ProviderToolDefinition>,
    mcp_tools: BTreeMap<String, McpToolBinding>,
    memory_prompt: Option<String>,
}

struct ProviderTurnOutput {
    text: String,
    reasoning: String,
    turn: ProviderTurn,
}

enum ExecuteFailure {
    Local,
    Provider(ProviderError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolCallOutcome {
    Continue,
    Cancelled,
    DeadlineExceeded,
    Shutdown,
}

struct ToolCallExecutionContext<'a> {
    workspace_root: Option<&'a std::path::Path>,
    workspace_id: Option<&'a str>,
    tool_control: ToolExecutionControl,
    cancellation: watch::Receiver<bool>,
    deadline: Instant,
}

enum ApprovalWaitOutcome {
    Approved,
    Denied,
    Cancelled,
    DeadlineExceeded,
    Shutdown,
}

enum ClarificationWaitOutcome {
    Answered(String),
    Cancelled,
    DeadlineExceeded,
    Shutdown,
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl RunService {
    pub(crate) fn new(
        profiles: Arc<ProfileService>,
        sessions: Arc<SessionService>,
        skills: Arc<SkillService>,
        memory: Arc<MemoryService>,
        web: Arc<WebService>,
    ) -> Self {
        Self::with_provider(
            profiles,
            sessions,
            skills,
            memory,
            web,
            Arc::new(OpenAiCompatibleProvider::new()),
        )
    }

    pub(crate) fn with_provider(
        profiles: Arc<ProfileService>,
        sessions: Arc<SessionService>,
        skills: Arc<SkillService>,
        memory: Arc<MemoryService>,
        web: Arc<WebService>,
        provider: Arc<dyn ProviderTransport>,
    ) -> Self {
        let browser = BrowserManager::new(profiles.hermes_home());
        Self::with_provider_and_browser(profiles, sessions, skills, memory, web, provider, browser)
    }

    fn with_provider_and_browser(
        profiles: Arc<ProfileService>,
        sessions: Arc<SessionService>,
        skills: Arc<SkillService>,
        memory: Arc<MemoryService>,
        web: Arc<WebService>,
        provider: Arc<dyn ProviderTransport>,
        browser: BrowserManager,
    ) -> Self {
        let owner_id = format!("runtime_{}", Uuid::new_v4().simple());
        let runtime_lease = match sessions.acquire_runtime_lease(&owner_id) {
            Ok(lease) => match sessions.recover_interrupted_runs() {
                Ok(_) => Some(lease),
                Err(error) => {
                    tracing::error!(?error, "failed to recover interrupted runs");
                    sessions.release_runtime_lease(&lease);
                    None
                }
            },
            Err(error) => {
                tracing::error!(?error, "failed to acquire the Run runtime lease");
                None
            }
        };
        let runtime_lease_valid = runtime_lease.is_some();
        let processes = ProcessManager::new(sessions.clone());
        let mcp = Arc::new(McpService::new(profiles.clone()));
        let service = Self {
            inner: Arc::new(RunServiceInner {
                profiles,
                sessions,
                skills,
                memory,
                web,
                browser,
                mcp,
                provider,
                tools: ToolRegistry::hermes_v0182(),
                processes,
                notifications: Mutex::new(HashMap::new()),
                tasks: Arc::new(TrackedTaskRegistry::new()),
                runtime_lease_valid: AtomicBool::new(runtime_lease_valid),
                shutting_down: AtomicBool::new(false),
                runtime_lease: Mutex::new(runtime_lease),
            }),
        };
        service.start_runtime_tasks();
        service
    }

    pub fn is_available(&self) -> bool {
        self.inner.sessions.is_available()
            && self.inner.runtime_lease_valid.load(Ordering::Acquire)
            && !self.inner.shutting_down.load(Ordering::Acquire)
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown_inner(ShutdownMode::Drain).await;
    }

    pub(crate) async fn shutdown_preserving_runs(&self) {
        self.shutdown_inner(ShutdownMode::PreserveRuns).await;
    }

    async fn shutdown_inner(&self, requested_mode: ShutdownMode) {
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let ticket = self.inner.tasks.begin_shutdown(requested_mode);
        self.inner.shutting_down.store(true, Ordering::Release);
        self.inner
            .runtime_lease_valid
            .store(false, Ordering::Release);
        if !ticket.is_leader() {
            if !self.inner.tasks.wait_stopped_until(&ticket, deadline).await {
                tracing::warn!("timed out waiting for the active shutdown leader");
            }
            return;
        }

        if !self.inner.tasks.wait_admissions_empty_until(deadline).await {
            tracing::warn!("timed out waiting for admitted Run operations during shutdown");
        }

        let tracked_runs = self.inner.tasks.snapshot_runs();
        if ticket.mode() == ShutdownMode::Drain {
            for tracked in &tracked_runs {
                let result = run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = tracked.run_id().to_owned();
                    move || sessions.request_run_cancel(&run_id)
                })
                .await;
                if let Err(error) = result {
                    tracing::warn!(
                        ?error,
                        run_id = %tracked.run_id(),
                        "failed to persist Run cancellation during shutdown"
                    );
                }
            }
        }
        let stop_reason = match ticket.mode() {
            ShutdownMode::Drain => StopReason::Drain,
            ShutdownMode::PreserveRuns => StopReason::Preserve,
        };
        for tracked in &tracked_runs {
            tracked.stop().request(stop_reason);
        }

        self.inner
            .processes
            .shutdown_all_until(deadline.into_std())
            .await;

        let grace_deadline = deadline.min(Instant::now() + SHUTDOWN_TASK_GRACE);
        let aborted_runs = if self.inner.tasks.wait_empty_until(grace_deadline).await {
            Vec::new()
        } else {
            self.inner.tasks.abort_all()
        };
        if !self.inner.tasks.wait_empty_until(deadline).await {
            tracing::warn!("timed out waiting for tracked runtime tasks during shutdown");
        }

        if ticket.mode() == ShutdownMode::Drain {
            for tracked in aborted_runs {
                let terminal = run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = tracked.run_id().to_owned();
                    move || {
                        sessions.cancel_run_terminal(&run_id, "cancelled during backend shutdown")
                    }
                })
                .await;
                match terminal {
                    Ok(run) => self.notify_terminal_run(&run.id).await,
                    Err(error) => tracing::warn!(
                        ?error,
                        run_id = %tracked.run_id(),
                        "failed to terminalize an aborted Run during shutdown"
                    ),
                }
            }
        }

        self.settle_async_tool_deliveries_for_shutdown(deadline)
            .await;
        self.inner.processes.release_shutdown_resources();
        self.close_all_notifications();
        if tokio::time::timeout_at(deadline, self.inner.browser.shutdown_all())
            .await
            .is_err()
        {
            tracing::warn!("timed out stopping browser resources during shutdown");
        }
        self.release_runtime_lease().await;
        if !self.inner.tasks.finish_shutdown(&ticket) {
            tracing::warn!("shutdown finished with tracked tasks or admissions still registered");
        }
    }

    async fn release_runtime_lease(&self) {
        let lease = self
            .inner
            .runtime_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(lease) = lease else {
            return;
        };
        let sessions = self.inner.sessions.clone();
        let _ = run_blocking(move || {
            sessions.release_runtime_lease(&lease);
            Ok(())
        })
        .await;
    }

    fn start_runtime_tasks(&self) {
        let Some(lease) = self
            .inner
            .runtime_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let _ = self.spawn_runtime_task(async move {
            loop {
                tokio::time::sleep(RUNTIME_LEASE_RENEW_INTERVAL).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner.shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let sessions = inner.sessions.clone();
                drop(inner);
                let lease_for_renewal = lease.clone();
                let renewed = tokio::task::spawn_blocking(move || {
                    sessions.renew_runtime_lease(&lease_for_renewal)
                })
                .await
                .is_ok_and(|result| result.is_ok());
                if renewed {
                    continue;
                }
                if let Some(inner) = weak.upgrade() {
                    inner.runtime_lease_valid.store(false, Ordering::Release);
                    for tracked in inner.tasks.snapshot_runs() {
                        tracked.stop().request(StopReason::Preserve);
                    }
                }
                return;
            }
        });
        let this = self.clone();
        let _ = self.spawn_runtime_task(async move {
            this.resume_queued_runs().await;
        });
        let this = self.clone();
        let _ = self.spawn_runtime_task(async move {
            this.resume_async_tool_deliveries().await;
        });
    }

    fn spawn_runtime_task<F>(&self, future: F) -> Result<(), RegistryError>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let admission = self.inner.tasks.begin_admission()?;
        let result = self
            .inner
            .tasks
            .spawn_tracked(&admission, None, future)
            .map(|_| ());
        admission.release();
        result
    }

    async fn resume_queued_runs(&self) {
        let sessions = match run_blocking({
            let sessions = self.inner.sessions.clone();
            move || sessions.queued_session_ids()
        })
        .await
        {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::error!(?error, "failed to enumerate queued Runs during recovery");
                return;
            }
        };
        for session_id in sessions {
            self.advance_queue_for_session(session_id).await;
        }
    }

    async fn resume_async_tool_deliveries(&self) {
        let deliveries = match run_blocking({
            let sessions = self.inner.sessions.clone();
            move || {
                sessions
                    .pending_async_tool_deliveries()
                    .map_err(|_| RunError::StorageUnavailable)
            }
        })
        .await
        {
            Ok(deliveries) => deliveries,
            Err(error) => {
                tracing::error!(?error, "failed to enumerate pending async tool deliveries");
                return;
            }
        };
        for delivery in deliveries {
            self.schedule_async_tool_delivery(delivery);
        }
    }

    async fn settle_async_tool_deliveries_for_shutdown(&self, deadline: Instant) {
        let settled = tokio::time::timeout_at(deadline, async {
            let deliveries = run_blocking({
                let sessions = self.inner.sessions.clone();
                move || {
                    sessions
                        .pending_async_tool_deliveries()
                        .map_err(|_| RunError::StorageUnavailable)
                }
            })
            .await?;
            for delivery in deliveries {
                let owner = ProcessOwner {
                    profile_id: delivery.process.profile_id.clone(),
                    session_id: delivery.process.session_id.clone(),
                };
                let view = match self
                    .inner
                    .processes
                    .poll(owner, &delivery.process.process_id)
                    .await
                {
                    Ok(view) => view,
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            process_id = %delivery.process.process_id,
                            "failed to observe async tool delivery during shutdown"
                        );
                        continue;
                    }
                };
                if !view.record.status.is_terminal() {
                    tracing::warn!(
                        process_id = %delivery.process.process_id,
                        "async tool delivery remained non-terminal during shutdown"
                    );
                    continue;
                }
                let trigger = match delivery.kind {
                    AsyncToolDeliveryKind::Completion => AsyncToolDeliveryTrigger::Completion,
                    AsyncToolDeliveryKind::Watch => {
                        let matched_pattern_count = delivery
                            .watch_patterns
                            .iter()
                            .filter(|pattern| view.output.contains(pattern.as_str()))
                            .count();
                        if matched_pattern_count == 0 {
                            AsyncToolDeliveryTrigger::WatchMissed
                        } else {
                            AsyncToolDeliveryTrigger::Watch {
                                matched_pattern_count: u8::try_from(matched_pattern_count)
                                    .unwrap_or(u8::MAX),
                            }
                        }
                    }
                };
                let settlement = run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let delivery = delivery.clone();
                    move || sessions.settle_async_tool_delivery(&delivery, trigger)
                })
                .await;
                match settlement {
                    Ok(AsyncToolDeliveryDisposition::Published)
                    | Ok(AsyncToolDeliveryDisposition::AlreadySettled) => {
                        self.notify_terminal_run(&delivery.process.creator_run_id)
                            .await;
                    }
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            process_id = %delivery.process.process_id,
                            "failed to settle async tool delivery during shutdown"
                        );
                    }
                }
            }
            Ok::<(), RunError>(())
        })
        .await;
        match settled {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    ?error,
                    "failed to enumerate async tool deliveries during shutdown"
                );
            }
            Err(_) => {
                tracing::warn!("timed out settling async tool deliveries during shutdown");
            }
        }
    }

    fn schedule_async_tool_delivery(&self, delivery: PendingAsyncToolDelivery) {
        let this = self.clone();
        let _ = self.spawn_runtime_task(async move {
            this.await_async_tool_delivery(delivery).await;
        });
    }

    async fn terminal_async_delivery_pending(
        &self,
        process_id: Option<&str>,
    ) -> Result<bool, RunError> {
        let Some(process_id) = process_id else {
            return Ok(false);
        };
        let process_id = process_id.to_owned();
        run_blocking({
            let sessions = self.inner.sessions.clone();
            move || {
                sessions
                    .pending_async_tool_deliveries()
                    .map_err(|_| RunError::StorageUnavailable)
                    .map(|deliveries| {
                        deliveries
                            .into_iter()
                            .any(|delivery| delivery.process.process_id == process_id)
                    })
            }
        })
        .await
    }

    async fn schedule_async_tool_delivery_for_process(&self, process_id: String) {
        let delivery = run_blocking({
            let sessions = self.inner.sessions.clone();
            move || {
                sessions
                    .pending_async_tool_deliveries()
                    .map_err(|_| RunError::StorageUnavailable)
                    .map(|deliveries| {
                        deliveries
                            .into_iter()
                            .find(|delivery| delivery.process.process_id == process_id)
                    })
            }
        })
        .await;
        match delivery {
            Ok(Some(delivery)) => self.schedule_async_tool_delivery(delivery),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(?error, "failed to schedule async tool delivery");
            }
        }
    }

    async fn await_async_tool_delivery(&self, delivery: PendingAsyncToolDelivery) {
        let owner = ProcessOwner {
            profile_id: delivery.process.profile_id.clone(),
            session_id: delivery.process.session_id.clone(),
        };
        loop {
            if !self.is_available() {
                return;
            }
            let view = match self
                .inner
                .processes
                .poll(owner.clone(), &delivery.process.process_id)
                .await
            {
                Ok(view) => view,
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        process_id = %delivery.process.process_id,
                        "failed to observe pending async tool delivery"
                    );
                    tokio::time::sleep(ASYNC_TOOL_DELIVERY_POLL_INTERVAL).await;
                    continue;
                }
            };
            let trigger = match delivery.kind {
                AsyncToolDeliveryKind::Completion if view.record.status.is_terminal() => {
                    Some(AsyncToolDeliveryTrigger::Completion)
                }
                AsyncToolDeliveryKind::Watch => {
                    let matched_pattern_count = delivery
                        .watch_patterns
                        .iter()
                        .filter(|pattern| view.output.contains(pattern.as_str()))
                        .count();
                    if matched_pattern_count > 0 {
                        Some(AsyncToolDeliveryTrigger::Watch {
                            matched_pattern_count: u8::try_from(matched_pattern_count)
                                .unwrap_or(u8::MAX),
                        })
                    } else if view.record.status.is_terminal() {
                        Some(AsyncToolDeliveryTrigger::WatchMissed)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(trigger) = trigger {
                let settlement = run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let delivery = delivery.clone();
                    move || sessions.settle_async_tool_delivery(&delivery, trigger)
                })
                .await;
                match settlement {
                    Ok(AsyncToolDeliveryDisposition::Published)
                    | Ok(AsyncToolDeliveryDisposition::AlreadySettled) => {
                        self.notify_terminal_run(&delivery.process.creator_run_id)
                            .await;
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            process_id = %delivery.process.process_id,
                            "failed to settle async tool delivery"
                        );
                    }
                }
            }
            tokio::time::sleep(ASYNC_TOOL_DELIVERY_POLL_INTERVAL).await;
        }
    }

    fn schedule_queue_advance(&self, session_id: String) {
        let this = self.clone();
        let _ = self.spawn_runtime_task(async move {
            this.advance_queue_for_session(session_id).await;
        });
    }

    async fn advance_queue_for_session(&self, session_id: String) {
        let admission = match self.inner.tasks.begin_admission() {
            Ok(admission) => admission,
            Err(_) => return,
        };
        if !self.is_available() {
            return;
        }
        let claim = match run_blocking({
            let sessions = self.inner.sessions.clone();
            let session_id = session_id.clone();
            move || sessions.claim_next_queued_run(&session_id)
        })
        .await
        {
            Ok(claim) => claim,
            Err(error) => {
                tracing::error!(?error, %session_id, "failed to claim the next queued Run");
                return;
            }
        };
        let Some(claim) = claim else {
            return;
        };
        self.notify(&claim.run.id);
        if self.launch_queued(&admission, claim.clone()).is_err() {
            self.fail_local(&claim.run.id).await;
        }
    }

    fn launch_queued(
        &self,
        admission: &AdmissionGuard,
        claim: QueuedRunClaim,
    ) -> Result<(), RegistryError> {
        let deadline = Instant::now() + MAX_RUN_DURATION;
        let stop = StopControl::new(ToolExecutionControl::new(deadline.into_std()));
        let tracked = TrackedRun::new(claim.run.id.clone(), stop.clone());
        let this = self.clone();
        self.inner
            .tasks
            .spawn_tracked(admission, Some(tracked), async move {
                this.prepare_and_execute_queued(claim, stop, deadline).await;
            })
            .map(|_| ())
    }

    async fn prepare_and_execute_queued(
        &self,
        claim: QueuedRunClaim,
        stop: StopControl,
        deadline: Instant,
    ) {
        let run_id = claim.run.id.clone();
        if stop.reason().is_some() {
            self.finish_for_stop(&run_id, &stop).await;
            return;
        }
        let browser_available = self.browser_available();
        let mut prepared = match run_blocking({
            let profiles = self.inner.profiles.clone();
            let sessions = self.inner.sessions.clone();
            let tools = self.inner.tools.clone();
            let memory = self.inner.memory.clone();
            let web = self.inner.web.clone();
            let session_id = claim.run.session_id.clone();
            let request = claim.request.clone();
            move || {
                prepare_model(
                    ModelPreparationDependencies {
                        profiles: &profiles,
                        sessions: &sessions,
                        memory: &memory,
                        web: &web,
                        tools: &tools,
                        browser_available,
                    },
                    &session_id,
                    &request,
                )
            }
        })
        .await
        {
            Ok(prepared) => prepared,
            Err(_) => {
                if stop.reason().is_some() {
                    self.finish_for_stop(&run_id, &stop).await;
                    return;
                }
                self.fail_local(&claim.run.id).await;
                return;
            }
        };
        let discovered = match self.inner.mcp.discover_tools(&prepared.profile_id).await {
            Ok(discovered) => discovered,
            Err(_) => {
                if stop.reason().is_some() {
                    self.finish_for_stop(&run_id, &stop).await;
                    return;
                }
                self.fail_local(&claim.run.id).await;
                return;
            }
        };
        for binding in discovered {
            let name = binding.provider_name().to_owned();
            if prepared.mcp_tools.contains_key(&name)
                || prepared
                    .tools
                    .iter()
                    .any(|definition| definition.name == name)
            {
                self.fail_local(&claim.run.id).await;
                return;
            }
            prepared.tools.push(binding.provider_definition());
            prepared.mcp_tools.insert(name, binding);
        }
        if stop.reason().is_some() {
            self.finish_for_stop(&run_id, &stop).await;
            return;
        }
        self.execute(claim.run, prepared, stop, deadline).await;
    }

    pub(crate) fn code_execution_available(&self) -> bool {
        self.is_available() && crate::code_execution::is_available()
    }

    pub(crate) fn browser_available(&self) -> bool {
        self.is_available() && self.inner.browser.is_available()
    }

    pub(crate) fn browser_downloads_available(&self) -> bool {
        self.is_available() && self.inner.browser.downloads_available()
    }

    pub async fn create_run(
        &self,
        session_id: String,
        request: CreateRun,
        idempotency_key: String,
    ) -> Result<RunAccepted, RunError> {
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        let replay = run_blocking({
            let sessions = self.inner.sessions.clone();
            let session_id = session_id.clone();
            let request = request.clone();
            let idempotency_key = idempotency_key.clone();
            move || sessions.lookup_run_replay(&session_id, &request, &idempotency_key)
        })
        .await?;
        if let Some(replay) = replay {
            return Ok(replay);
        }
        if !request.message.file_ids.is_empty() {
            return Err(RunError::CapabilityMissing);
        }

        let browser_available = self.browser_available();
        let mut prepared = run_blocking({
            let profiles = self.inner.profiles.clone();
            let sessions = self.inner.sessions.clone();
            let tools = self.inner.tools.clone();
            let memory = self.inner.memory.clone();
            let web = self.inner.web.clone();
            let session_id = session_id.clone();
            let request = request.clone();
            move || {
                prepare_model(
                    ModelPreparationDependencies {
                        profiles: &profiles,
                        sessions: &sessions,
                        memory: &memory,
                        web: &web,
                        tools: &tools,
                        browser_available,
                    },
                    &session_id,
                    &request,
                )
            }
        })
        .await?;
        let discovered = self
            .inner
            .mcp
            .discover_tools(&prepared.profile_id)
            .await
            .map_err(|_| RunError::CapabilityMissing)?;
        for binding in discovered {
            let name = binding.provider_name().to_owned();
            if prepared.mcp_tools.contains_key(&name)
                || prepared
                    .tools
                    .iter()
                    .any(|definition| definition.name == name)
            {
                return Err(RunError::CapabilityMissing);
            }
            prepared.tools.push(binding.provider_definition());
            prepared.mcp_tools.insert(name, binding);
        }
        let admission = self
            .inner
            .tasks
            .begin_admission()
            .map_err(|_| RunError::EngineUnavailable)?;
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        let model_label = prepared.model_label.clone();
        let accepted = run_blocking({
            let sessions = self.inner.sessions.clone();
            let session_id = session_id.clone();
            let request = request.clone();
            let idempotency_key = idempotency_key.clone();
            move || sessions.create_run(&session_id, &request, &idempotency_key, &model_label)
        })
        .await?;

        if !accepted.run.status.is_terminal() {
            self.notify(&accepted.run.id);
        }
        if accepted.run.status == RunStatus::Running
            && !matches!(accepted.disposition, super::RunDisposition::Replayed)
            && self
                .launch(&admission, accepted.run.clone(), prepared)
                .is_err()
        {
            self.fail_local(&accepted.run.id).await;
            return Err(RunError::EngineUnavailable);
        }
        Ok(accepted)
    }

    pub async fn get_run(&self, run_id: String) -> Result<Run, RunError> {
        run_blocking({
            let sessions = self.inner.sessions.clone();
            move || sessions.get_run(&run_id)
        })
        .await
    }

    pub async fn list_active_runs(
        &self,
        profile_id: String,
        session_id: Option<String>,
    ) -> Result<ActiveRunList, RunError> {
        run_blocking({
            let sessions = self.inner.sessions.clone();
            move || sessions.list_active_runs(&profile_id, session_id.as_deref())
        })
        .await
    }

    pub async fn resolve_approval(
        &self,
        run_id: String,
        approval_id: String,
        decision: ApprovalDecision,
    ) -> Result<ActionAccepted, RunError> {
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        decision.validate()?;
        let stored_decision = match decision.decision {
            ApprovalChoice::Once => ToolApprovalDecision::Once,
            ApprovalChoice::Session => ToolApprovalDecision::Session,
            ApprovalChoice::Always => ToolApprovalDecision::Always,
            ApprovalChoice::Deny => ToolApprovalDecision::Deny,
        };
        let reason = decision.reason;
        let _admission = self
            .inner
            .tasks
            .begin_admission()
            .map_err(|_| RunError::EngineUnavailable)?;
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        let result = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.clone();
            let approval_id = approval_id.clone();
            move || {
                sessions
                    .resolve_tool_approval(
                        &run_id,
                        &approval_id,
                        stored_decision,
                        reason.as_deref(),
                    )
                    .map_err(run_from_tool_approval)
            }
        })
        .await;
        if result.is_ok() || matches!(result, Err(RunError::ApprovalExpired)) {
            self.notify(&run_id);
        }
        result.map(|_| ActionAccepted::accepted())
    }

    pub async fn answer_clarification(
        &self,
        run_id: String,
        request_id: String,
        answer: ClarificationAnswer,
    ) -> Result<ActionAccepted, RunError> {
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        answer.validate()?;
        let answer = answer.answer;
        let _admission = self
            .inner
            .tasks
            .begin_admission()
            .map_err(|_| RunError::EngineUnavailable)?;
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        let result = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.clone();
            let request_id = request_id.clone();
            move || {
                sessions
                    .resolve_clarification(&run_id, &request_id, &answer)
                    .map_err(run_from_clarification)
            }
        })
        .await;
        if result.is_ok() {
            self.notify(&run_id);
        }
        result.map(|_| ActionAccepted::accepted())
    }

    pub(crate) async fn event_batch(
        &self,
        run_id: String,
        after_sequence: u64,
    ) -> Result<RunEventBatch, RunError> {
        let batch = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.clone();
            move || sessions.run_event_batch(&run_id, after_sequence)
        })
        .await;
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                self.close_notifications(&run_id);
                return Err(error);
            }
        };
        if batch.terminal {
            self.close_notifications(&run_id);
        }
        Ok(batch)
    }

    pub(crate) fn subscribe(&self, run_id: &str) -> broadcast::Receiver<()> {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            let (sender, receiver) = broadcast::channel(1);
            drop(sender);
            return receiver;
        }
        self.notification_sender(run_id).subscribe()
    }

    pub async fn cancel_run(&self, run_id: String) -> Result<Run, RunError> {
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        let _admission = self
            .inner
            .tasks
            .begin_admission()
            .map_err(|_| RunError::EngineUnavailable)?;
        if !self.is_available() {
            return Err(RunError::EngineUnavailable);
        }
        let (run, disposition) = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.clone();
            move || sessions.request_run_cancel(&run_id)
        })
        .await?;
        if disposition == CancelDisposition::AlreadyTerminal {
            return Ok(run);
        }
        if disposition == CancelDisposition::CancelledQueued {
            self.close_notifications(&run_id);
            self.schedule_queue_advance(run.session_id.clone());
            return Ok(run);
        }
        self.notify(&run_id);
        if let Some(stop) = self.inner.tasks.control_for_run(&run_id) {
            stop.request(StopReason::UserCancel);
            Ok(run)
        } else {
            let cancelled = run_blocking({
                let sessions = self.inner.sessions.clone();
                let run_id = run_id.clone();
                move || sessions.cancel_run_terminal(&run_id, "cancelled by user")
            })
            .await?;
            self.inner
                .browser
                .cleanup_run(&cancelled.profile_id, &cancelled.id)
                .await;
            self.notify_terminal_run(&run_id).await;
            Ok(cancelled)
        }
    }

    fn launch(
        &self,
        admission: &AdmissionGuard,
        run: Run,
        prepared: PreparedModel,
    ) -> Result<(), RegistryError> {
        let deadline = Instant::now() + MAX_RUN_DURATION;
        let stop = StopControl::new(ToolExecutionControl::new(deadline.into_std()));
        let tracked = TrackedRun::new(run.id.clone(), stop.clone());
        let this = self.clone();
        self.inner
            .tasks
            .spawn_tracked(admission, Some(tracked), async move {
                this.execute(run, prepared, stop, deadline).await;
            })
            .map(|_| ())
    }

    async fn execute(
        &self,
        run: Run,
        prepared: PreparedModel,
        stop: StopControl,
        deadline: Instant,
    ) {
        let run_id = run.id.clone();
        let tool_control = stop.tool_control();
        let cancel_receiver = stop.subscribe();

        match self.get_run(run_id.clone()).await {
            Ok(current) if current.status == RunStatus::Running => {}
            Ok(current)
                if matches!(current.status, RunStatus::Cancelling | RunStatus::Cancelled) =>
            {
                stop.request(StopReason::UserCancel);
                self.finish_for_stop(&run_id, &stop).await;
                return;
            }
            Ok(current) if current.status.is_terminal() => {
                return;
            }
            Ok(_) | Err(_) => {
                self.fail_local(&run_id).await;
                return;
            }
        }

        let workspace = match run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.clone();
            move || sessions.workspace_for_run(&run_id)
        })
        .await
        {
            Ok(workspace) => workspace,
            Err(_) => {
                self.fail_local(&run_id).await;
                return;
            }
        };
        let workspace_root = workspace.as_ref().map(|workspace| workspace.path.as_path());
        let workspace_id = workspace.as_ref().map(|workspace| workspace.id.as_str());
        let message_id = format!("message_{}", Uuid::new_v4().simple());
        if run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.clone();
            let message_id = message_id.clone();
            move || sessions.begin_assistant_message(&run_id, &message_id)
        })
        .await
        .is_err()
        {
            self.fail_local(&run_id).await;
            return;
        }
        self.notify(&run_id);

        let mut total_usage = ProviderUsage::default();
        let mut total_tool_calls = 0usize;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut public_tool_calls = Vec::new();

        for turn_index in 1..=MAX_PROVIDER_TURNS_PER_RUN {
            if Instant::now() >= deadline {
                self.fail_provider(&run_id, ProviderError::Timeout).await;
                return;
            }
            if *cancel_receiver.borrow() {
                self.finish_for_stop(&run_id, &stop).await;
                return;
            }

            let mut context = match run_blocking({
                let sessions = self.inner.sessions.clone();
                let run_id = run_id.clone();
                move || sessions.provider_continuation_context(&run_id)
            })
            .await
            {
                Ok(context) => context,
                Err(_) => {
                    self.fail_local(&run_id).await;
                    return;
                }
            };
            if let Some(prompt) = prepared.memory_prompt.as_ref() {
                context.insert(
                    0,
                    ProviderContextMessage::System {
                        content: prompt.clone(),
                    },
                );
            }
            let request = ProviderRequest {
                provider_id: prepared.provider_id.clone(),
                model: prepared.model.clone(),
                base_url: prepared.base_url.clone(),
                url: prepared.url.clone(),
                secret: prepared.secret.clone(),
                reasoning_effort: prepared.reasoning_effort.clone(),
                messages: context.into_iter().map(provider_context_message).collect(),
                tools: prepared.tools.clone(),
            };
            let output = match self
                .execute_provider_turn(
                    &run_id,
                    &message_id,
                    request,
                    cancel_receiver.clone(),
                    &total_usage,
                    deadline,
                )
                .await
            {
                Ok(output) => output,
                Err(ExecuteFailure::Provider(ProviderError::Cancelled)) => {
                    self.finish_for_stop(&run_id, &stop).await;
                    return;
                }
                Err(ExecuteFailure::Provider(error)) => {
                    self.fail_provider(&run_id, error).await;
                    return;
                }
                Err(ExecuteFailure::Local) => {
                    self.fail_local(&run_id).await;
                    return;
                }
            };

            text.push_str(&output.text);
            reasoning.push_str(&output.reasoning);
            total_usage = match checked_usage_add(&total_usage, &output.turn.usage) {
                Ok(usage) => usage,
                Err(error) => {
                    self.fail_provider(&run_id, error).await;
                    return;
                }
            };

            let (finish, planned_calls) = match &output.turn.finish {
                ProviderFinish::Stop => (ProviderTurnFinish::Stop, Vec::new()),
                ProviderFinish::Length => (ProviderTurnFinish::Length, Vec::new()),
                ProviderFinish::ContentFilter => (ProviderTurnFinish::ContentFilter, Vec::new()),
                ProviderFinish::ToolCalls(calls) => {
                    total_tool_calls = match total_tool_calls.checked_add(calls.len()) {
                        Some(total) if total <= MAX_TOOL_CALLS_PER_RUN => total,
                        _ => {
                            self.fail_provider(&run_id, ProviderError::InvalidResponse)
                                .await;
                            return;
                        }
                    };
                    (
                        ProviderTurnFinish::ToolCalls,
                        calls
                            .iter()
                            .map(|call| RawToolCallPlan {
                                call_id: call.id.clone(),
                                tool_name: call.name.clone(),
                                arguments_json: call.arguments_json.clone(),
                            })
                            .collect(),
                    )
                }
            };
            let plan = ProviderTurnPlan {
                turn_index,
                assistant_message_id: message_id.clone(),
                content: (!output.text.is_empty()).then_some(output.text.clone()),
                reasoning: (!output.reasoning.is_empty()).then_some(output.reasoning.clone()),
                finish,
                usage: provider_usage(&output.turn.usage),
                tool_calls: planned_calls,
            };
            if run_blocking({
                let sessions = self.inner.sessions.clone();
                let run_id = run_id.clone();
                move || sessions.record_provider_turn(&run_id, &plan).map(|_| ())
            })
            .await
            .is_err()
            {
                self.fail_local(&run_id).await;
                return;
            }

            match output.turn.finish {
                ProviderFinish::ToolCalls(calls) => {
                    for call in calls {
                        if *cancel_receiver.borrow() {
                            self.finish_for_stop(&run_id, &stop).await;
                            return;
                        }
                        let tool_context = ToolCallExecutionContext {
                            workspace_root,
                            workspace_id,
                            tool_control: tool_control.clone(),
                            cancellation: cancel_receiver.clone(),
                            deadline,
                        };
                        let execution = if let Some(binding) = prepared.mcp_tools.get(&call.name) {
                            self.execute_mcp_tool_call(
                                &run,
                                &call,
                                binding,
                                tool_context,
                                &mut public_tool_calls,
                            )
                            .await
                        } else {
                            self.execute_tool_call(
                                &run,
                                &call,
                                tool_context,
                                &mut public_tool_calls,
                            )
                            .await
                        };
                        let outcome = match execution {
                            Ok(outcome) => outcome,
                            Err(_) => {
                                if *cancel_receiver.borrow() {
                                    self.finish_for_stop(&run_id, &stop).await;
                                    return;
                                }
                                let current = self.get_run(run_id.clone()).await.ok();
                                if current.as_ref().is_some_and(|current| {
                                    matches!(
                                        current.status,
                                        RunStatus::Cancelling | RunStatus::Cancelled
                                    )
                                }) {
                                    self.finish_cancelled(&run_id).await;
                                } else {
                                    self.fail_local(&run_id).await;
                                }
                                return;
                            }
                        };
                        self.notify(&run_id);
                        match outcome {
                            ToolCallOutcome::Continue => {}
                            ToolCallOutcome::Cancelled => {
                                self.finish_for_stop(&run_id, &stop).await;
                                return;
                            }
                            ToolCallOutcome::DeadlineExceeded => {
                                self.fail_provider(&run_id, ProviderError::Timeout).await;
                                return;
                            }
                            ToolCallOutcome::Shutdown => {
                                return;
                            }
                        }
                    }
                }
                ProviderFinish::Stop if !text.is_empty() || !reasoning.is_empty() => {
                    let usage = provider_usage(&total_usage);
                    let reasoning = (!reasoning.is_empty()).then_some(reasoning);
                    let model_label = prepared.model_label.clone();
                    let completed = run_blocking({
                        let sessions = self.inner.sessions.clone();
                        let run_id = run_id.clone();
                        let message_id = message_id.clone();
                        move || {
                            sessions.complete_run(
                                &run_id,
                                CompleteRunPlan {
                                    message_id,
                                    text,
                                    reasoning,
                                    tool_calls: public_tool_calls,
                                    usage,
                                    model_label,
                                },
                            )
                        }
                    })
                    .await;
                    let completed = match completed {
                        Ok(completed) => completed,
                        Err(_) => {
                            if *cancel_receiver.borrow() {
                                self.finish_for_stop(&run_id, &stop).await;
                                return;
                            }
                            let current = self.get_run(run_id.clone()).await.ok();
                            if current
                                .as_ref()
                                .is_some_and(|current| current.status == RunStatus::Cancelling)
                            {
                                self.finish_cancelled(&run_id).await;
                            } else {
                                self.fail_local(&run_id).await;
                            }
                            return;
                        }
                    };
                    self.inner
                        .browser
                        .cleanup_run(&completed.profile_id, &completed.id)
                        .await;
                    self.notify_terminal_run(&run_id).await;
                    self.schedule_queue_advance(completed.session_id);
                    return;
                }
                ProviderFinish::Stop | ProviderFinish::Length | ProviderFinish::ContentFilter => {
                    self.fail_provider(&run_id, ProviderError::InvalidResponse)
                        .await;
                    return;
                }
            }
        }

        self.fail_provider(&run_id, ProviderError::InvalidResponse)
            .await;
    }

    async fn execute_provider_turn(
        &self,
        run_id: &str,
        message_id: &str,
        request: ProviderRequest,
        cancel_receiver: watch::Receiver<bool>,
        base_usage: &ProviderUsage,
        deadline: Instant,
    ) -> Result<ProviderTurnOutput, ExecuteFailure> {
        let (event_sender, mut event_receiver) = mpsc::channel(PROVIDER_EVENT_CAPACITY);
        let provider = self.inner.provider.clone();
        let mut provider_task = tokio::spawn(async move {
            provider
                .stream_chat(request, event_sender, cancel_receiver)
                .await
        });
        let _provider_abort = AbortTaskOnDrop(provider_task.abort_handle());
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut latest_turn_usage = ProviderUsage::default();

        loop {
            let event = tokio::select! {
                event = event_receiver.recv() => event,
                _ = tokio::time::sleep_until(deadline) => {
                    provider_task.abort();
                    return Err(ExecuteFailure::Provider(ProviderError::Timeout));
                }
            };
            let Some(event) = event else { break };
            let persisted = match event {
                ProviderEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    self.persist_delta(run_id, "message.delta", message_id, delta)
                        .await
                }
                ProviderEvent::ReasoningDelta(delta) => {
                    reasoning.push_str(&delta);
                    self.persist_delta(run_id, "reasoning.delta", message_id, delta)
                        .await
                }
                ProviderEvent::Usage(usage) => {
                    let cumulative =
                        checked_usage_add(base_usage, &usage).map_err(ExecuteFailure::Provider)?;
                    latest_turn_usage = usage;
                    self.persist_usage(run_id, provider_usage(&cumulative))
                        .await
                }
            };
            persisted.map_err(|_| ExecuteFailure::Local)?;
            self.notify(run_id);
        }

        let turn = tokio::time::timeout_at(deadline, &mut provider_task)
            .await
            .map_err(|_| {
                provider_task.abort();
                ExecuteFailure::Provider(ProviderError::Timeout)
            })?
            .map_err(|_| ExecuteFailure::Provider(ProviderError::InvalidResponse))?
            .map_err(ExecuteFailure::Provider)?;
        if turn.usage != latest_turn_usage {
            let cumulative =
                checked_usage_add(base_usage, &turn.usage).map_err(ExecuteFailure::Provider)?;
            self.persist_usage(run_id, provider_usage(&cumulative))
                .await
                .map_err(|_| ExecuteFailure::Local)?;
            self.notify(run_id);
        }
        Ok(ProviderTurnOutput {
            text,
            reasoning,
            turn,
        })
    }

    async fn execute_mcp_tool_call(
        &self,
        run: &Run,
        call: &crate::providers::ProviderToolCall,
        binding: &McpToolBinding,
        context: ToolCallExecutionContext<'_>,
        public_tool_calls: &mut Vec<ToolCall>,
    ) -> Result<ToolCallOutcome, RunError> {
        let ToolCallExecutionContext {
            workspace_id,
            tool_control,
            cancellation,
            deadline,
            ..
        } = context;
        let input_summary = format!("MCP tool {}", binding.provider_name());
        let validation = binding.validate_arguments(&call.arguments_json);
        let started = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run.id.clone();
            let call_id = call.id.clone();
            let name = call.name.clone();
            let input_summary = input_summary.clone();
            move || {
                sessions.start_tool_invocation_with_event(&run_id, &call_id, &name, &input_summary)
            }
        })
        .await?;
        self.notify(&run.id);

        if validation.is_err() {
            let problem = RunProblem::tool(&run.id, &call.id);
            run_blocking({
                let sessions = self.inner.sessions.clone();
                let run_id = run.id.clone();
                let call_id = call.id.clone();
                move || {
                    sessions
                        .fail_tool_invocation_with_event(
                            &run_id,
                            &call_id,
                            started.checkpoint,
                            r#"{"code":"tool_invalid_arguments","retryable":false}"#,
                            r#"{"ok":false,"error":{"code":"tool_invalid_arguments"}}"#,
                            &problem,
                        )
                        .map(|_| ())
                }
            })
            .await?;
            public_tool_calls.push(ToolCall {
                call_id: call.id.clone(),
                name: call.name.clone(),
                status: ToolCallStatus::Failed,
                input_summary: Some(input_summary),
                result_summary: Some("MCP tool arguments were invalid".to_owned()),
                artifacts: Vec::new(),
            });
            return Ok(ToolCallOutcome::Continue);
        }

        let arguments_sha256: [u8; 32] = Sha256::digest(call.arguments_json.as_bytes()).into();
        let approval = self
            .wait_for_tool_approval(
                run,
                call,
                &input_summary,
                ToolApprovalExecutionBinding {
                    run_id: run.id.clone(),
                    profile_id: run.profile_id.clone(),
                    session_id: run.session_id.clone(),
                    workspace_id: workspace_id.map(ToOwned::to_owned),
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    invocation_checkpoint: started.checkpoint,
                    arguments_sha256,
                },
                cancellation,
                deadline,
            )
            .await?;
        match approval {
            ApprovalWaitOutcome::Denied => {
                public_tool_calls.push(ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    status: ToolCallStatus::Failed,
                    input_summary: Some(input_summary),
                    result_summary: Some("Tool execution denied".to_owned()),
                    artifacts: Vec::new(),
                });
                return Ok(ToolCallOutcome::Continue);
            }
            ApprovalWaitOutcome::Cancelled => return Ok(ToolCallOutcome::Cancelled),
            ApprovalWaitOutcome::DeadlineExceeded => {
                return Ok(ToolCallOutcome::DeadlineExceeded);
            }
            ApprovalWaitOutcome::Shutdown => return Ok(ToolCallOutcome::Shutdown),
            ApprovalWaitOutcome::Approved => {}
        }

        let execution = self
            .inner
            .mcp
            .call_tool(
                &run.profile_id,
                binding,
                &call.arguments_json,
                &tool_control,
            )
            .await;
        let outcome = match execution {
            Ok(output) => {
                run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = run.id.clone();
                    let call_id = call.id.clone();
                    let raw_result_json = output.raw_result_json.clone();
                    let provider_content = output.provider_content.clone();
                    let result_summary = output.result_summary.clone();
                    move || {
                        sessions
                            .complete_tool_invocation_with_event(
                                &run_id,
                                &call_id,
                                started.checkpoint,
                                &raw_result_json,
                                &provider_content,
                                &result_summary,
                            )
                            .map(|_| ())
                    }
                })
                .await?;
                public_tool_calls.push(ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    status: ToolCallStatus::Completed,
                    input_summary: Some(output.input_summary),
                    result_summary: Some(output.result_summary),
                    artifacts: Vec::new(),
                });
                ToolCallOutcome::Continue
            }
            Err(error) => {
                let (raw_error, provider_content, result_summary, outcome) =
                    mcp_error_projection(error);
                let problem = RunProblem::tool(&run.id, &call.id);
                run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = run.id.clone();
                    let call_id = call.id.clone();
                    let raw_error = raw_error.clone();
                    let provider_content = provider_content.clone();
                    move || {
                        sessions
                            .fail_tool_invocation_with_event(
                                &run_id,
                                &call_id,
                                started.checkpoint,
                                &raw_error,
                                &provider_content,
                                &problem,
                            )
                            .map(|_| ())
                    }
                })
                .await?;
                public_tool_calls.push(ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    status: ToolCallStatus::Failed,
                    input_summary: Some(input_summary),
                    result_summary: Some(result_summary),
                    artifacts: Vec::new(),
                });
                outcome
            }
        };
        Ok(outcome)
    }

    async fn execute_tool_call(
        &self,
        run: &Run,
        call: &crate::providers::ProviderToolCall,
        context: ToolCallExecutionContext<'_>,
        public_tool_calls: &mut Vec<ToolCall>,
    ) -> Result<ToolCallOutcome, RunError> {
        let ToolCallExecutionContext {
            workspace_root,
            workspace_id,
            tool_control,
            cancellation,
            deadline,
        } = context;
        let preparation = tokio::task::spawn_blocking({
            let registry = self.inner.tools.clone();
            let profiles = self.inner.profiles.clone();
            let sessions = self.inner.sessions.clone();
            let skills = self.inner.skills.clone();
            let memory = self.inner.memory.clone();
            let web = self.inner.web.clone();
            let browser = self.inner.browser.clone();
            let workspace_root = workspace_root.map(std::path::Path::to_owned);
            let workspace_id = workspace_id.map(ToOwned::to_owned);
            let profile_id = run.profile_id.clone();
            let session_id = run.session_id.clone();
            let run_id = run.id.clone();
            let call_id = call.id.clone();
            let name = call.name.clone();
            let arguments = call.arguments_json.clone();
            let tool_control = tool_control.clone();
            move || {
                let context = ToolExecutionContext::new(
                    &profiles,
                    &sessions,
                    &skills,
                    workspace_root.as_deref(),
                    &profile_id,
                    tool_control,
                )
                .with_memory(&memory)
                .with_web(&web)
                .with_browser(&browser)
                .with_async_tool_delivery()
                .with_run_owner(
                    &session_id,
                    workspace_id.as_deref(),
                    &run_id,
                    &call_id,
                );
                registry.prepare(&context, &name, &arguments)
            }
        })
        .await
        .map_err(|_| RunError::StorageUnavailable)?;
        let input_summary = preparation
            .as_ref()
            .map(|prepared| prepared.input_summary.clone())
            .unwrap_or_else(|_| call.name.clone());
        let approval_summary = preparation
            .as_ref()
            .ok()
            .and_then(|prepared| prepared.approval_summary.clone())
            .unwrap_or_else(|| input_summary.clone());
        let started = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run.id.clone();
            let call_id = call.id.clone();
            let name = call.name.clone();
            let input_summary = input_summary.clone();
            move || {
                sessions.start_tool_invocation_with_event(&run_id, &call_id, &name, &input_summary)
            }
        })
        .await?;
        self.notify(&run.id);
        let approval_binding =
            preparation
                .as_ref()
                .ok()
                .map(|prepared| ToolApprovalExecutionBinding {
                    run_id: run.id.clone(),
                    profile_id: run.profile_id.clone(),
                    session_id: run.session_id.clone(),
                    workspace_id: workspace_id.map(ToOwned::to_owned),
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    invocation_checkpoint: started.checkpoint,
                    arguments_sha256: prepared.arguments_sha256(),
                });

        if let Some(clarification) = preparation
            .as_ref()
            .ok()
            .and_then(PreparedToolCall::clarification)
            .cloned()
        {
            let binding = ClarificationContinuationBinding {
                run_id: run.id.clone(),
                call_id: call.id.clone(),
                invocation_checkpoint: started.checkpoint,
                arguments_sha256: preparation
                    .as_ref()
                    .map_err(|_| RunError::DataInvalid)?
                    .arguments_sha256(),
            };
            return match self
                .wait_for_clarification(
                    run,
                    call,
                    &clarification,
                    binding,
                    cancellation.clone(),
                    deadline,
                )
                .await?
            {
                ClarificationWaitOutcome::Answered(answer) => {
                    let private_result = serde_json::to_string(&serde_json::json!({
                        "answer": answer,
                    }))
                    .map_err(|_| RunError::DataInvalid)?;
                    run_blocking({
                        let sessions = self.inner.sessions.clone();
                        let run_id = run.id.clone();
                        let call_id = call.id.clone();
                        let private_result = private_result.clone();
                        move || {
                            sessions
                                .complete_tool_invocation_with_event(
                                    &run_id,
                                    &call_id,
                                    started.checkpoint,
                                    &private_result,
                                    &private_result,
                                    "Clarification answered",
                                )
                                .map(|_| ())
                        }
                    })
                    .await?;
                    public_tool_calls.push(ToolCall {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        status: ToolCallStatus::Completed,
                        input_summary: Some(input_summary),
                        result_summary: Some("Clarification answered".to_owned()),
                        artifacts: Vec::new(),
                    });
                    Ok(ToolCallOutcome::Continue)
                }
                ClarificationWaitOutcome::Cancelled => Ok(ToolCallOutcome::Cancelled),
                ClarificationWaitOutcome::DeadlineExceeded => Ok(ToolCallOutcome::DeadlineExceeded),
                ClarificationWaitOutcome::Shutdown => Ok(ToolCallOutcome::Shutdown),
            };
        }

        let approved_once = match &preparation {
            Ok(PreparedToolCall {
                risk: ToolRisk::ApprovalRequired,
                ..
            }) => match self
                .wait_for_tool_approval(
                    run,
                    call,
                    &approval_summary,
                    approval_binding.clone().ok_or(RunError::DataInvalid)?,
                    cancellation.clone(),
                    deadline,
                )
                .await?
            {
                ApprovalWaitOutcome::Approved => true,
                ApprovalWaitOutcome::Denied => {
                    public_tool_calls.push(ToolCall {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        status: ToolCallStatus::Failed,
                        input_summary: Some(input_summary),
                        result_summary: Some("Tool execution denied".to_owned()),
                        artifacts: Vec::new(),
                    });
                    return Ok(ToolCallOutcome::Continue);
                }
                ApprovalWaitOutcome::Cancelled => return Ok(ToolCallOutcome::Cancelled),
                ApprovalWaitOutcome::DeadlineExceeded => {
                    return Ok(ToolCallOutcome::DeadlineExceeded);
                }
                ApprovalWaitOutcome::Shutdown => return Ok(ToolCallOutcome::Shutdown),
            },
            Ok(PreparedToolCall {
                risk: ToolRisk::ReadOnly,
                ..
            })
            | Err(_) => false,
        };

        let execution = match preparation {
            Err(error) => Err(error),
            Ok(prepared) if ToolRegistry::requires_async_execution(&prepared) => {
                let context = ToolExecutionContext::new(
                    &self.inner.profiles,
                    &self.inner.sessions,
                    &self.inner.skills,
                    workspace_root,
                    &run.profile_id,
                    tool_control.clone(),
                )
                .with_web(&self.inner.web)
                .with_browser(&self.inner.browser)
                .with_async_tool_delivery()
                .with_run_owner(&run.session_id, workspace_id, &run.id, &call.id);
                let context = if approved_once {
                    context.with_once_approval()
                } else {
                    context
                };
                self.inner
                    .tools
                    .execute_prepared_async(
                        &context,
                        &self.inner.processes,
                        &self.inner.web,
                        ProcessExecutionContext {
                            profile_id: run.profile_id.clone(),
                            session_id: run.session_id.clone(),
                            workspace_id: workspace_id.map(ToOwned::to_owned),
                            workspace_root: workspace_root.map(std::path::Path::to_owned),
                            creator_run_id: run.id.clone(),
                            call_id: call.id.clone(),
                        },
                        &call.name,
                        &call.arguments_json,
                        &prepared,
                        cancellation,
                        deadline.into_std(),
                    )
                    .await
            }
            Ok(prepared) => tokio::task::spawn_blocking({
                let registry = self.inner.tools.clone();
                let profiles = self.inner.profiles.clone();
                let sessions = self.inner.sessions.clone();
                let skills = self.inner.skills.clone();
                let memory = self.inner.memory.clone();
                let web = self.inner.web.clone();
                let browser = self.inner.browser.clone();
                let workspace_root = workspace_root.map(std::path::Path::to_owned);
                let workspace_id = workspace_id.map(ToOwned::to_owned);
                let profile_id = run.profile_id.clone();
                let session_id = run.session_id.clone();
                let run_id = run.id.clone();
                let call_id = call.id.clone();
                let name = call.name.clone();
                let arguments = call.arguments_json.clone();
                let tool_control = tool_control.clone();
                move || {
                    let context = ToolExecutionContext::new(
                        &profiles,
                        &sessions,
                        &skills,
                        workspace_root.as_deref(),
                        &profile_id,
                        tool_control,
                    )
                    .with_memory(&memory)
                    .with_web(&web)
                    .with_browser(&browser)
                    .with_async_tool_delivery()
                    .with_run_owner(
                        &session_id,
                        workspace_id.as_deref(),
                        &run_id,
                        &call_id,
                    );
                    let context = if approved_once {
                        context.with_once_approval()
                    } else {
                        context
                    };
                    registry.execute_prepared(&context, &name, &arguments, &prepared)
                }
            })
            .await
            .map_err(|_| RunError::StorageUnavailable)?,
        };

        let outcome = match execution {
            Ok(output) => {
                let async_delivery_process_id = output.async_delivery_process_id.clone();
                let async_delivery_pending = self
                    .terminal_async_delivery_pending(async_delivery_process_id.as_deref())
                    .await?;
                run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = run.id.clone();
                    let call_id = call.id.clone();
                    let raw_result_json = output.raw_result_json.clone();
                    let provider_content = output.provider_content.clone();
                    let result_summary = output.result_summary.clone();
                    move || {
                        sessions
                            .complete_tool_invocation_with_event_and_async_delivery(
                                &run_id,
                                &call_id,
                                started.checkpoint,
                                &raw_result_json,
                                &provider_content,
                                &result_summary,
                                async_delivery_pending,
                            )
                            .map(|_| ())
                    }
                })
                .await?;
                if let Some(process_id) = async_delivery_process_id.as_deref() {
                    self.inner
                        .processes
                        .commit_launch(
                            ProcessOwner {
                                profile_id: run.profile_id.clone(),
                                session_id: run.session_id.clone(),
                            },
                            process_id,
                        )
                        .await
                        .map_err(|_| RunError::StorageUnavailable)?;
                    self.schedule_async_tool_delivery_for_process(process_id.to_owned())
                        .await;
                }
                public_tool_calls.push(ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    status: ToolCallStatus::Completed,
                    input_summary: Some(output.input_summary),
                    result_summary: Some(output.result_summary),
                    artifacts: Vec::new(),
                });
                ToolCallOutcome::Continue
            }
            Err(error) => {
                let (raw_error, provider_content, result_summary, outcome) = match error {
                    ToolExecutionError::Cancelled => (
                        r#"{"code":"tool_cancelled","retryable":true}"#.to_owned(),
                        r#"{"ok":false,"error":{"code":"tool_cancelled"}}"#.to_owned(),
                        "Tool execution cancelled".to_owned(),
                        ToolCallOutcome::Cancelled,
                    ),
                    ToolExecutionError::DeadlineExceeded => (
                        r#"{"code":"tool_deadline_exceeded","retryable":true}"#.to_owned(),
                        r#"{"ok":false,"error":{"code":"tool_deadline_exceeded"}}"#.to_owned(),
                        "Tool execution deadline exceeded".to_owned(),
                        ToolCallOutcome::DeadlineExceeded,
                    ),
                    _ => (
                        r#"{"code":"tool_failed","retryable":false}"#.to_owned(),
                        r#"{"ok":false,"error":{"code":"tool_failed"}}"#.to_owned(),
                        "Tool execution failed".to_owned(),
                        ToolCallOutcome::Continue,
                    ),
                };
                let problem = RunProblem::tool(&run.id, &call.id);
                run_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = run.id.clone();
                    let call_id = call.id.clone();
                    let problem = problem.clone();
                    move || {
                        sessions
                            .fail_tool_invocation_with_event(
                                &run_id,
                                &call_id,
                                started.checkpoint,
                                &raw_error,
                                &provider_content,
                                &problem,
                            )
                            .map(|_| ())
                    }
                })
                .await?;
                public_tool_calls.push(ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    status: ToolCallStatus::Failed,
                    input_summary: Some(input_summary),
                    result_summary: Some(result_summary),
                    artifacts: Vec::new(),
                });
                outcome
            }
        };
        Ok(outcome)
    }

    async fn wait_for_tool_approval(
        &self,
        run: &Run,
        call: &crate::providers::ProviderToolCall,
        input_summary: &str,
        execution_binding: ToolApprovalExecutionBinding,
        mut cancellation: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<ApprovalWaitOutcome, RunError> {
        let mut notifications = self.subscribe(&run.id);
        let remaining = deadline.saturating_duration_since(Instant::now());
        let approval_window = remaining
            .min(APPROVAL_TIMEOUT)
            .max(Duration::from_millis(50));
        let expires_at = (OffsetDateTime::now_utc()
            + TimeDuration::try_from(approval_window).map_err(|_| RunError::DataInvalid)?)
        .format(&Rfc3339)
        .map_err(|_| RunError::DataInvalid)?;
        let approval_id = format!("approval_{}", Uuid::new_v4().simple());
        let request = ToolApprovalRequest {
            approval_id: approval_id.clone(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            input_summary: Some(input_summary.to_owned()),
            choices: vec![ToolApprovalDecision::Once, ToolApprovalDecision::Deny],
            expires_at,
        };
        run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run.id.clone();
            move || {
                sessions
                    .request_tool_approval_with_event(&run_id, &request)
                    .map_err(run_from_tool_approval)
            }
        })
        .await?;
        self.notify(&run.id);

        loop {
            if *cancellation.borrow() {
                return Ok(ApprovalWaitOutcome::Cancelled);
            }
            if self.inner.shutting_down.load(Ordering::Acquire) {
                return Ok(ApprovalWaitOutcome::Shutdown);
            }
            let approval = run_blocking({
                let sessions = self.inner.sessions.clone();
                let run_id = run.id.clone();
                let approval_id = approval_id.clone();
                move || {
                    sessions
                        .load_tool_approval(&run_id, &approval_id)
                        .map_err(run_from_tool_approval)
                }
            })
            .await?;
            if approval.state == ToolApprovalState::Resolved {
                return match (approval.decision, approval.resolved_by) {
                    (Some(ToolApprovalDecision::Once), Some(ToolApprovalResolvedBy::User)) => {
                        let claimed = tokio::task::spawn_blocking({
                            let sessions = self.inner.sessions.clone();
                            let run_id = run.id.clone();
                            let approval_id = approval_id.clone();
                            let execution_binding = execution_binding.clone();
                            move || {
                                sessions.claim_tool_approval(
                                    &run_id,
                                    &approval_id,
                                    &execution_binding,
                                )
                            }
                        })
                        .await
                        .map_err(|_| RunError::StorageUnavailable)?;
                        match claimed {
                            Ok(_) => Ok(ApprovalWaitOutcome::Approved),
                            Err(ToolApprovalError::ExecutionNotAuthorized) => {
                                let current = self.get_run(run.id.clone()).await?;
                                if matches!(
                                    current.status,
                                    RunStatus::Cancelling | RunStatus::Cancelled
                                ) {
                                    Ok(ApprovalWaitOutcome::Cancelled)
                                } else {
                                    Err(RunError::DataInvalid)
                                }
                            }
                            Err(error) => Err(run_from_tool_approval(error)),
                        }
                    }
                    (
                        Some(ToolApprovalDecision::Deny),
                        Some(ToolApprovalResolvedBy::User | ToolApprovalResolvedBy::Expiry),
                    ) => {
                        if approval.resolved_by == Some(ToolApprovalResolvedBy::Expiry)
                            && Instant::now() >= deadline
                        {
                            Ok(ApprovalWaitOutcome::DeadlineExceeded)
                        } else {
                            Ok(ApprovalWaitOutcome::Denied)
                        }
                    }
                    (
                        Some(ToolApprovalDecision::Deny),
                        Some(ToolApprovalResolvedBy::Cancellation),
                    ) => Ok(ApprovalWaitOutcome::Cancelled),
                    _ => Err(RunError::DataInvalid),
                };
            }

            let now_unix_ms = i64::try_from(
                OffsetDateTime::now_utc()
                    .unix_timestamp_nanos()
                    .div_euclid(1_000_000),
            )
            .map_err(|_| RunError::DataInvalid)?;
            if now_unix_ms >= approval.expires_at_unix_ms {
                let expired = tokio::task::spawn_blocking({
                    let sessions = self.inner.sessions.clone();
                    let run_id = run.id.clone();
                    let approval_id = approval_id.clone();
                    move || sessions.expire_tool_approval(&run_id, &approval_id)
                })
                .await
                .map_err(|_| RunError::StorageUnavailable)?;
                match expired {
                    Ok(_) => self.notify(&run.id),
                    Err(ToolApprovalError::NotExpired) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => return Err(run_from_tool_approval(error)),
                }
                continue;
            }
            let wait_ms = u64::try_from(approval.expires_at_unix_ms - now_unix_ms)
                .map_err(|_| RunError::DataInvalid)?
                .min(5_000);
            tokio::select! {
                notification = notifications.recv() => {
                    if notification.is_err() {
                        if *cancellation.borrow() {
                            return Ok(ApprovalWaitOutcome::Cancelled);
                        }
                        if self.inner.shutting_down.load(Ordering::Acquire) {
                            return Ok(ApprovalWaitOutcome::Shutdown);
                        }
                        return Err(RunError::EngineUnavailable);
                    }
                }
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() {
                        return Ok(ApprovalWaitOutcome::Cancelled);
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(wait_ms.max(1))) => {}
            }
        }
    }

    async fn wait_for_clarification(
        &self,
        run: &Run,
        call: &crate::providers::ProviderToolCall,
        clarification: &PreparedClarification,
        continuation_binding: ClarificationContinuationBinding,
        mut cancellation: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<ClarificationWaitOutcome, RunError> {
        let mut notifications = self.subscribe(&run.id);
        let request_id = format!("clarification_{}", Uuid::new_v4().simple());
        let request = ClarificationRequest {
            request_id: request_id.clone(),
            call_id: call.id.clone(),
            question: clarification.question.clone(),
            choices: clarification.choices.clone(),
        };
        run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run.id.clone();
            move || {
                sessions
                    .request_clarification_with_event(&run_id, &request)
                    .map_err(run_from_clarification)
            }
        })
        .await?;
        self.notify(&run.id);

        loop {
            if *cancellation.borrow() {
                return Ok(ClarificationWaitOutcome::Cancelled);
            }
            if self.inner.shutting_down.load(Ordering::Acquire) {
                return Ok(ClarificationWaitOutcome::Shutdown);
            }
            let stored = run_blocking({
                let sessions = self.inner.sessions.clone();
                let run_id = run.id.clone();
                let request_id = request_id.clone();
                move || {
                    sessions
                        .load_clarification(&run_id, &request_id)
                        .map_err(run_from_clarification)
                }
            })
            .await?;
            if stored.state == ClarificationState::Resolved {
                return match stored.resolved_by {
                    Some(ClarificationResolvedBy::User) => {
                        let claimed = tokio::task::spawn_blocking({
                            let sessions = self.inner.sessions.clone();
                            let run_id = run.id.clone();
                            let request_id = request_id.clone();
                            let continuation_binding = continuation_binding.clone();
                            move || {
                                sessions.claim_clarification_answer(
                                    &run_id,
                                    &request_id,
                                    &continuation_binding,
                                )
                            }
                        })
                        .await
                        .map_err(|_| RunError::StorageUnavailable)?;
                        match claimed {
                            Ok(claimed) => claimed
                                .answer
                                .map(ClarificationWaitOutcome::Answered)
                                .ok_or(RunError::DataInvalid),
                            Err(ClarificationError::ContinuationNotAuthorized) => {
                                let current = self.get_run(run.id.clone()).await?;
                                if matches!(
                                    current.status,
                                    RunStatus::Cancelling | RunStatus::Cancelled
                                ) {
                                    Ok(ClarificationWaitOutcome::Cancelled)
                                } else {
                                    Err(RunError::DataInvalid)
                                }
                            }
                            Err(error) => Err(run_from_clarification(error)),
                        }
                    }
                    Some(ClarificationResolvedBy::Cancellation) => {
                        Ok(ClarificationWaitOutcome::Cancelled)
                    }
                    Some(ClarificationResolvedBy::Failure) | None => Err(RunError::DataInvalid),
                };
            }
            if Instant::now() >= deadline {
                return Ok(ClarificationWaitOutcome::DeadlineExceeded);
            }

            tokio::select! {
                notification = notifications.recv() => {
                    if notification.is_err() {
                        if *cancellation.borrow() {
                            return Ok(ClarificationWaitOutcome::Cancelled);
                        }
                        if self.inner.shutting_down.load(Ordering::Acquire) {
                            return Ok(ClarificationWaitOutcome::Shutdown);
                        }
                        return Err(RunError::EngineUnavailable);
                    }
                }
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() {
                        return Ok(ClarificationWaitOutcome::Cancelled);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(ClarificationWaitOutcome::DeadlineExceeded);
                }
            }
        }
    }

    async fn finish_for_stop(&self, run_id: &str, stop: &StopControl) {
        match stop.reason() {
            Some(StopReason::Preserve) => {}
            Some(StopReason::Drain) => {
                self.finish_cancelled_with_reason(run_id, "cancelled during backend shutdown")
                    .await;
            }
            None | Some(StopReason::UserCancel) => {
                self.finish_cancelled(run_id).await;
            }
        }
    }

    async fn finish_cancelled(&self, run_id: &str) {
        self.finish_cancelled_with_reason(run_id, "cancelled by user")
            .await;
    }

    async fn finish_cancelled_with_reason(&self, run_id: &str, reason: &'static str) {
        let terminal = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.to_owned();
            move || sessions.cancel_run_terminal(&run_id, reason)
        })
        .await
        .ok();
        self.notify_terminal_run(run_id).await;
        if let Some(terminal) = terminal {
            self.inner
                .browser
                .cleanup_run(&terminal.profile_id, &terminal.id)
                .await;
            self.schedule_queue_advance(terminal.session_id);
        }
    }

    async fn persist_delta(
        &self,
        run_id: &str,
        event_name: &str,
        message_id: &str,
        delta: String,
    ) -> Result<Run, RunError> {
        run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.to_owned();
            let event_name = event_name.to_owned();
            let message_id = message_id.to_owned();
            move || sessions.append_run_delta(&run_id, &event_name, &message_id, &delta)
        })
        .await
    }

    async fn persist_usage(&self, run_id: &str, usage: Usage) -> Result<Run, RunError> {
        run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.to_owned();
            move || sessions.update_run_usage(&run_id, &usage)
        })
        .await
    }

    async fn fail_local(&self, run_id: &str) {
        let problem = RunProblem::local_failure(run_id);
        let terminal = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.to_owned();
            move || sessions.fail_run(&run_id, &problem)
        })
        .await
        .ok();
        self.notify_terminal_run(run_id).await;
        if let Some(terminal) = terminal {
            self.inner
                .browser
                .cleanup_run(&terminal.profile_id, &terminal.id)
                .await;
            self.schedule_queue_advance(terminal.session_id);
        }
    }

    async fn fail_provider(&self, run_id: &str, error: ProviderError) {
        let problem = RunProblem::engine(run_id, &error);
        let terminal = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.to_owned();
            move || sessions.fail_run(&run_id, &problem)
        })
        .await
        .ok();
        self.notify_terminal_run(run_id).await;
        if let Some(terminal) = terminal {
            self.inner
                .browser
                .cleanup_run(&terminal.profile_id, &terminal.id)
                .await;
            self.schedule_queue_advance(terminal.session_id);
        }
    }

    fn notification_sender(&self, run_id: &str) -> broadcast::Sender<()> {
        let mut notifications = self
            .inner
            .notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        notifications
            .entry(run_id.to_owned())
            .or_insert_with(|| broadcast::channel(EVENT_NOTIFICATION_CAPACITY).0)
            .clone()
    }

    fn notify(&self, run_id: &str) {
        let sender = self
            .inner
            .notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(run_id)
            .cloned();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }

    async fn notify_terminal_run(&self, run_id: &str) {
        let terminal = run_blocking({
            let sessions = self.inner.sessions.clone();
            let run_id = run_id.to_owned();
            move || {
                let run = sessions.get_run(&run_id)?;
                sessions
                    .run_event_batch(&run_id, run.last_sequence)
                    .map(|batch| batch.terminal)
            }
        })
        .await;
        match terminal {
            Ok(false) => self.notify(run_id),
            Ok(true) | Err(_) => self.close_notifications(run_id),
        }
    }

    fn close_notifications(&self, run_id: &str) {
        let sender = self
            .inner
            .notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(run_id);
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }

    fn close_all_notifications(&self) {
        let senders = {
            let mut notifications = self
                .inner
                .notifications
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *notifications)
        };
        for sender in senders.into_values() {
            let _ = sender.send(());
        }
    }
}

fn mcp_error_projection(error: McpRuntimeError) -> (String, String, String, ToolCallOutcome) {
    match error {
        McpRuntimeError::Cancelled => (
            r#"{"code":"tool_cancelled","retryable":true}"#.to_owned(),
            r#"{"ok":false,"error":{"code":"tool_cancelled"}}"#.to_owned(),
            "MCP tool execution cancelled".to_owned(),
            ToolCallOutcome::Cancelled,
        ),
        McpRuntimeError::Timeout => (
            r#"{"code":"tool_deadline_exceeded","retryable":true}"#.to_owned(),
            r#"{"ok":false,"error":{"code":"tool_deadline_exceeded"}}"#.to_owned(),
            "MCP tool execution timed out".to_owned(),
            ToolCallOutcome::DeadlineExceeded,
        ),
        McpRuntimeError::InvalidArguments => (
            r#"{"code":"tool_invalid_arguments","retryable":false}"#.to_owned(),
            r#"{"ok":false,"error":{"code":"tool_invalid_arguments"}}"#.to_owned(),
            "MCP tool arguments were invalid".to_owned(),
            ToolCallOutcome::Continue,
        ),
        McpRuntimeError::Configuration
        | McpRuntimeError::Transport
        | McpRuntimeError::InvalidProtocol
        | McpRuntimeError::InvalidResult => (
            r#"{"code":"tool_failed","retryable":false}"#.to_owned(),
            r#"{"ok":false,"error":{"code":"tool_failed"}}"#.to_owned(),
            "MCP tool execution failed".to_owned(),
            ToolCallOutcome::Continue,
        ),
    }
}

struct ModelPreparationDependencies<'a> {
    profiles: &'a ProfileService,
    sessions: &'a SessionService,
    memory: &'a MemoryService,
    web: &'a WebService,
    tools: &'a ToolRegistry,
    browser_available: bool,
}

fn prepare_model(
    dependencies: ModelPreparationDependencies<'_>,
    session_id: &str,
    request: &CreateRun,
) -> Result<PreparedModel, RunError> {
    let ModelPreparationDependencies {
        profiles,
        sessions,
        memory,
        web,
        tools,
        browser_available,
    } = dependencies;
    let session = sessions.get_session(session_id).map_err(run_from_session)?;
    let profile_id = session.value.profile_id;
    let profile = profiles.get_config(&profile_id).map_err(run_from_profile)?;
    let memory_snapshot = memory.snapshot(&profile_id).map_err(run_from_memory)?;
    let web_readiness = web
        .readiness(&profile_id)
        .unwrap_or(crate::web::WebReadiness {
            search_ready: false,
            extract_ready: false,
        });
    let tool_definitions = tools.definitions_for_profile_capabilities(
        &profile.value,
        request.workspace_id.is_some(),
        memory_snapshot.enabled,
        web_readiness,
        browser_available,
    );
    let configured = profile.value.model;
    let RunModelConfig {
        provider,
        model,
        base_url,
        reasoning_effort: model_reasoning,
    } = request.model_override.clone().unwrap_or(RunModelConfig {
        provider: configured.provider,
        model: configured.model,
        base_url: configured.base_url,
        reasoning_effort: configured.reasoning_effort,
    });
    if provider.is_empty()
        || model.is_empty()
        || provider.len() > 128
        || model.chars().count() > 500
        || provider.chars().any(char::is_control)
        || model.chars().any(char::is_control)
    {
        return Err(RunError::EngineUnavailable);
    }
    let reasoning_effort = request.reasoning_effort.clone().or(model_reasoning);
    if reasoning_effort
        .as_deref()
        .is_some_and(|value| !matches!(value, "minimal" | "low" | "medium" | "high" | "xhigh"))
    {
        return Err(RunError::InvalidRequest);
    }

    let catalog = profiles.providers();
    let requested = catalog
        .iter()
        .find(|entry| entry.id == provider)
        .ok_or(RunError::EngineUnavailable)?;
    let (provider_id, default_base_url, secret) = if provider == "auto" {
        let secret = profiles
            .first_secret_snapshot(&profile_id, &requested.secret_names, true)
            .map_err(run_from_profile)?
            .ok_or(RunError::EngineUnavailable)?;
        let selected = if secret.0 == "OPENROUTER_API_KEY" {
            "openrouter"
        } else {
            "openai-api"
        };
        let selected_entry = catalog
            .iter()
            .find(|entry| entry.id == selected)
            .ok_or(RunError::EngineUnavailable)?;
        (
            selected.to_owned(),
            selected_entry.default_base_url.clone(),
            Some(secret.1),
        )
    } else {
        if !OPENAI_COMPATIBLE_PROVIDERS.contains(&provider.as_str()) {
            return Err(RunError::EngineUnavailable);
        }
        let secret = profiles
            .first_secret_snapshot(
                &profile_id,
                &requested.secret_names,
                requested.requires_secret,
            )
            .map_err(run_from_profile)?;
        if requested.requires_secret && secret.is_none() {
            return Err(RunError::EngineUnavailable);
        }
        (
            provider.clone(),
            requested.default_base_url.clone(),
            secret.map(|(_, value)| value),
        )
    };
    let base_url = base_url
        .or(default_base_url)
        .ok_or(RunError::EngineUnavailable)?;
    let base_url = Url::parse(&base_url).map_err(|_| RunError::EngineUnavailable)?;
    if !matches!(base_url.scheme(), "http" | "https")
        || base_url.host_str().is_none()
        || !base_url.username().is_empty()
        || base_url.password().is_some()
        || base_url.query().is_some()
        || base_url.fragment().is_some()
    {
        return Err(RunError::EngineUnavailable);
    }
    let mut url = base_url.clone();
    let path = format!("{}/chat/completions", base_url.path().trim_end_matches('/'));
    url.set_path(&path);
    let model_label = format!("{provider_id}/{model}");
    Ok(PreparedModel {
        profile_id,
        provider_id,
        model,
        base_url,
        url,
        secret,
        reasoning_effort,
        model_label,
        tools: tool_definitions,
        mcp_tools: BTreeMap::new(),
        memory_prompt: memory_snapshot.prompt,
    })
}

fn provider_usage(usage: &ProviderUsage) -> Usage {
    Usage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage
            .total_tokens
            .max(usage.prompt_tokens.saturating_add(usage.completion_tokens)),
        cost: usage.cost,
    }
}

fn provider_context_message(message: ProviderContextMessage) -> ProviderMessage {
    match message {
        ProviderContextMessage::System { content } => ProviderMessage::System { content },
        ProviderContextMessage::User { content } => ProviderMessage::User { content },
        ProviderContextMessage::Assistant {
            content,
            tool_calls,
        } => ProviderMessage::assistant(
            content,
            tool_calls
                .into_iter()
                .map(|call| crate::providers::ProviderToolCall {
                    id: call.call_id,
                    name: call.tool_name,
                    arguments_json: call.arguments_json,
                })
                .collect(),
        ),
        ProviderContextMessage::Tool {
            tool_call_id,
            content,
        } => ProviderMessage::tool(tool_call_id, content),
    }
}

fn checked_usage_add(
    accumulated: &ProviderUsage,
    turn: &ProviderUsage,
) -> Result<ProviderUsage, ProviderError> {
    let prompt_tokens = accumulated
        .prompt_tokens
        .checked_add(turn.prompt_tokens)
        .ok_or(ProviderError::InvalidResponse)?;
    let completion_tokens = accumulated
        .completion_tokens
        .checked_add(turn.completion_tokens)
        .ok_or(ProviderError::InvalidResponse)?;
    let total_tokens = accumulated
        .total_tokens
        .checked_add(turn.total_tokens)
        .ok_or(ProviderError::InvalidResponse)?;
    let cost = match (accumulated.cost, turn.cost) {
        (Some(left), Some(right)) => {
            let value = left + right;
            if !value.is_finite() {
                return Err(ProviderError::InvalidResponse);
            }
            Some(value)
        }
        (None, Some(value)) if accumulated.total_tokens == 0 => Some(value),
        (Some(value), None) if turn.total_tokens == 0 => Some(value),
        _ => None,
    };
    Ok(ProviderUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cost,
    })
}

fn run_from_profile(error: ProfileError) -> RunError {
    match error {
        ProfileError::ProfileNotFound => RunError::NotFound,
        ProfileError::SecretStorageUnavailable => RunError::SecretStorageUnavailable,
        _ => RunError::EngineUnavailable,
    }
}

fn run_from_memory(error: MemoryError) -> RunError {
    match error {
        MemoryError::Profile(ProfileError::ProfileNotFound) => RunError::NotFound,
        MemoryError::Profile(_) | MemoryError::ProviderUnsupported { .. } => {
            RunError::EngineUnavailable
        }
        MemoryError::Storage(_) | MemoryError::DataInvalid | MemoryError::UnsafePath => {
            RunError::StorageUnavailable
        }
        _ => RunError::DataInvalid,
    }
}

fn run_from_tool_approval(error: ToolApprovalError) -> RunError {
    match error {
        ToolApprovalError::InvalidRequest => RunError::InvalidApprovalRequest,
        ToolApprovalError::NotFound => RunError::ApprovalNotFound,
        ToolApprovalError::ChoiceNotOffered => RunError::ApprovalChoiceNotOffered,
        ToolApprovalError::DecisionConflict => RunError::ApprovalDecisionConflict,
        ToolApprovalError::Expired => RunError::ApprovalExpired,
        ToolApprovalError::NoLongerPending => RunError::ApprovalNoLongerPending,
        ToolApprovalError::StorageBusy => RunError::StorageBusy,
        ToolApprovalError::StorageUnavailable => RunError::StorageUnavailable,
        ToolApprovalError::RequestConflict
        | ToolApprovalError::NotExpired
        | ToolApprovalError::ExecutionNotAuthorized
        | ToolApprovalError::ExecutionAlreadyClaimed
        | ToolApprovalError::DataInvalid => RunError::DataInvalid,
    }
}

fn run_from_clarification(error: ClarificationError) -> RunError {
    match error {
        ClarificationError::InvalidRequest => RunError::InvalidClarificationRequest,
        ClarificationError::NotFound => RunError::ClarificationNotFound,
        ClarificationError::ChoiceNotOffered => RunError::ClarificationChoiceNotOffered,
        ClarificationError::AnswerConflict => RunError::ClarificationAnswerConflict,
        ClarificationError::NoLongerPending => RunError::ClarificationNoLongerPending,
        ClarificationError::StorageBusy => RunError::StorageBusy,
        ClarificationError::StorageUnavailable => RunError::StorageUnavailable,
        ClarificationError::RequestConflict
        | ClarificationError::ContinuationNotAuthorized
        | ClarificationError::ContinuationAlreadyClaimed
        | ClarificationError::DataInvalid => RunError::DataInvalid,
    }
}

fn run_from_session(error: SessionError) -> RunError {
    match error {
        SessionError::NotFound => RunError::NotFound,
        SessionError::Archived => RunError::SessionArchived,
        SessionError::Busy => RunError::SessionBusy,
        SessionError::StorageBusy => RunError::StorageBusy,
        SessionError::StorageUnavailable | SessionError::DataInvalid => {
            RunError::StorageUnavailable
        }
        _ => RunError::InvalidRequest,
    }
}

async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, RunError> + Send + 'static,
) -> Result<T, RunError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| RunError::StorageUnavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::process_store::{
        AsyncToolDeliveryRequest, CreateProcess, ProcessStatus, ProcessTransition,
    };
    use serde_json::Value as JsonValue;
    use std::{
        net::Ipv4Addr,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TOKEN: &str = "01234567890123456789012345678901";
    const BROWSER_NAVIGATE_CALL_ID: &str = "call-browser-navigate-e2e";
    const BROWSER_SNAPSHOT_CALL_ID: &str = "call-browser-snapshot-e2e";
    const BROWSER_DOWNLOAD_CALL_ID: &str = "call-browser-download-e2e";

    struct DropSignal(watch::Sender<bool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.send_replace(true);
        }
    }

    struct NeverRespondingProvider {
        entered: watch::Sender<bool>,
        dropped: watch::Sender<bool>,
        calls: AtomicUsize,
    }

    impl NeverRespondingProvider {
        fn new() -> (Arc<Self>, watch::Receiver<bool>, watch::Receiver<bool>) {
            let (entered, entered_receiver) = watch::channel(false);
            let (dropped, dropped_receiver) = watch::channel(false);
            (
                Arc::new(Self {
                    entered,
                    dropped,
                    calls: AtomicUsize::new(0),
                }),
                entered_receiver,
                dropped_receiver,
            )
        }
    }

    #[async_trait::async_trait]
    impl ProviderTransport for NeverRespondingProvider {
        async fn stream_chat(
            &self,
            _request: ProviderRequest,
            events: mpsc::Sender<ProviderEvent>,
            _cancelled: watch::Receiver<bool>,
        ) -> Result<ProviderTurn, ProviderError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.entered.send_replace(true);
            let _keep_alive = (events, DropSignal(self.dropped.clone()));
            std::future::pending::<Result<ProviderTurn, ProviderError>>().await
        }
    }

    struct ImmediateProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ProviderTransport for ImmediateProvider {
        async fn stream_chat(
            &self,
            _request: ProviderRequest,
            events: mpsc::Sender<ProviderEvent>,
            _cancelled: watch::Receiver<bool>,
        ) -> Result<ProviderTurn, ProviderError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            events
                .send(ProviderEvent::TextDelta(
                    "recovered queue completed".to_owned(),
                ))
                .await
                .map_err(|_| ProviderError::Cancelled)?;
            Ok(ProviderTurn {
                usage: ProviderUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: None,
                },
                finish: ProviderFinish::Stop,
            })
        }
    }

    fn service_with_provider(
        home: &Path,
        sessions: Arc<SessionService>,
        provider: Arc<dyn ProviderTransport>,
    ) -> RunService {
        let profiles = Arc::new(ProfileService::without_credential_store(home.to_owned()));
        let current = profiles.get_config("default").unwrap();
        profiles
            .update_config(
                "default",
                &current.etag,
                &serde_json::json!({
                    "model": {
                        "provider": "lmstudio",
                        "model": "shutdown-test",
                        "baseUrl": "http://127.0.0.1:1/v1"
                    }
                }),
            )
            .unwrap();
        let skills = Arc::new(SkillService::new(profiles.clone(), TOKEN));
        let memory = Arc::new(MemoryService::new(profiles.clone(), TOKEN));
        let web = Arc::new(
            WebService::new(profiles.clone())
                .unwrap_or_else(|_| WebService::unavailable(profiles.clone())),
        );
        RunService::with_provider(profiles, sessions, skills, memory, web, provider)
    }

    async fn wait_for_true(receiver: &mut watch::Receiver<bool>) {
        if *receiver.borrow() {
            return;
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                receiver.changed().await.unwrap();
                if *receiver.borrow() {
                    return;
                }
            }
        })
        .await
        .expect("test signal should arrive");
    }

    struct BrowserDownloadProvider {
        page_url: String,
        calls: AtomicUsize,
        download_projection: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl ProviderTransport for BrowserDownloadProvider {
        async fn stream_chat(
            &self,
            request: ProviderRequest,
            events: mpsc::Sender<ProviderEvent>,
            _cancelled: watch::Receiver<bool>,
        ) -> Result<ProviderTurn, ProviderError> {
            assert!(request.tools.iter().any(|tool| {
                tool.name == "browser_download"
                    && tool.strict == Some(true)
                    && tool.parameters["required"] == serde_json::json!(["selector", "snapshotId"])
            }));
            let turn = self.calls.fetch_add(1, Ordering::SeqCst);
            let usage = ProviderUsage {
                prompt_tokens: 4,
                completion_tokens: 1,
                total_tokens: 5,
                cost: None,
            };
            events
                .send(ProviderEvent::Usage(usage.clone()))
                .await
                .map_err(|_| ProviderError::Cancelled)?;
            let finish = match turn {
                0 => ProviderFinish::ToolCalls(vec![crate::providers::ProviderToolCall {
                    id: BROWSER_NAVIGATE_CALL_ID.to_owned(),
                    name: "browser_navigate".to_owned(),
                    arguments_json: serde_json::json!({"url": self.page_url}).to_string(),
                }]),
                1 => ProviderFinish::ToolCalls(vec![crate::providers::ProviderToolCall {
                    id: BROWSER_SNAPSHOT_CALL_ID.to_owned(),
                    name: "browser_snapshot".to_owned(),
                    arguments_json: "{}".to_owned(),
                }]),
                2 => {
                    let snapshot_id = request
                        .messages
                        .last()
                        .and_then(|message| match message {
                            ProviderMessage::Tool {
                                tool_call_id,
                                content,
                            } if tool_call_id == BROWSER_SNAPSHOT_CALL_ID => {
                                serde_json::from_str::<JsonValue>(content).ok()
                            }
                            _ => None,
                        })
                        .and_then(|value| value["snapshotId"].as_str().map(ToOwned::to_owned))
                        .ok_or(ProviderError::InvalidResponse)?;
                    ProviderFinish::ToolCalls(vec![crate::providers::ProviderToolCall {
                        id: BROWSER_DOWNLOAD_CALL_ID.to_owned(),
                        name: "browser_download".to_owned(),
                        arguments_json: serde_json::json!({
                            "selector": "#download",
                            "snapshotId": snapshot_id,
                        })
                        .to_string(),
                    }])
                }
                3 => {
                    let content = request
                        .messages
                        .last()
                        .and_then(|message| match message {
                            ProviderMessage::Tool {
                                tool_call_id,
                                content,
                            } if tool_call_id == BROWSER_DOWNLOAD_CALL_ID => Some(content.clone()),
                            _ => None,
                        })
                        .ok_or(ProviderError::InvalidResponse)?;
                    *self.download_projection.lock().unwrap() = Some(content);
                    events
                        .send(ProviderEvent::TextDelta(
                            "Browser download completed safely".to_owned(),
                        ))
                        .await
                        .map_err(|_| ProviderError::Cancelled)?;
                    ProviderFinish::Stop
                }
                _ => return Err(ProviderError::InvalidResponse),
            };
            Ok(ProviderTurn { usage, finish })
        }
    }

    fn browser_download_file_count(home: &std::path::Path) -> usize {
        let profiles = home.join(".synthchat/browser/profiles");
        let Ok(profile_entries) = std::fs::read_dir(profiles) else {
            return 0;
        };
        profile_entries
            .flatten()
            .flat_map(|profile| {
                std::fs::read_dir(profile.path())
                    .into_iter()
                    .flatten()
                    .flatten()
            })
            .filter_map(|run| std::fs::read_dir(run.path().join("downloads")).ok())
            .flat_map(|entries| entries.flatten())
            .count()
    }

    #[tokio::test]
    async fn approved_browser_download_run_returns_only_safe_metadata_and_cleans_storage() {
        let Some(executable) = crate::browser::test_browser_binary() else {
            return;
        };
        let fixture = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = fixture.local_addr().unwrap();
        let (shutdown, mut shutdown_requested) = tokio::sync::oneshot::channel::<()>();
        let browser_server = tokio::spawn(async move {
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
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!doctype html><title>Run download fixture</title><a id=\"download\" href=\"/report.txt\">Download</a>".as_slice()
                            };
                            let _ = stream.write_all(response).await;
                        });
                    }
                }
            }
        });

        let home = tempfile::tempdir().unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let current = profiles.get_config("default").unwrap();
        profiles
            .update_config(
                "default",
                &current.etag,
                &serde_json::json!({
                    "model": {
                        "provider": "lmstudio",
                        "model": "browser-download-test",
                        "baseUrl": "http://127.0.0.1:1/v1"
                    },
                    "toolsets": {"browser": true}
                }),
            )
            .unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Approved browser download".to_owned()),
                },
                "approved-browser-download-session",
            )
            .unwrap();
        let skills = Arc::new(SkillService::new(profiles.clone(), TOKEN));
        let memory = Arc::new(MemoryService::new(profiles.clone(), TOKEN));
        let web = Arc::new(
            WebService::new(profiles.clone())
                .unwrap_or_else(|_| WebService::unavailable(profiles.clone())),
        );
        let provider = Arc::new(BrowserDownloadProvider {
            page_url: format!("http://127.0.0.1:{}/", address.port()),
            calls: AtomicUsize::new(0),
            download_projection: Mutex::new(None),
        });
        let browser = BrowserManager::with_test_binary(home.path(), executable);
        let service = RunService::with_provider_and_browser(
            profiles,
            sessions.clone(),
            skills,
            memory,
            web,
            provider.clone(),
            browser,
        );
        let accepted = service
            .create_run(
                session.value.id.clone(),
                CreateRun {
                    client_request_id: "approved-browser-download-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "download the report".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                "approved-browser-download-idempotency".to_owned(),
            )
            .await
            .unwrap();
        let run_id = accepted.run.id;
        let (approval_id, pending_call_id, choices) =
            tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    let run = service.get_run(run_id.clone()).await.unwrap();
                    if let Some(crate::runs::PendingAction::Approval {
                        approval_id,
                        call_id,
                        choices,
                        ..
                    }) = run.pending_action
                    {
                        break (approval_id, call_id, choices);
                    }
                    assert_ne!(run.status, RunStatus::Failed);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("browser download should wait for durable approval");
        assert_eq!(pending_call_id, BROWSER_DOWNLOAD_CALL_ID);
        assert_eq!(choices, vec!["once".to_owned(), "deny".to_owned()]);
        assert_eq!(browser_download_file_count(home.path()), 0);

        service
            .resolve_approval(
                run_id.clone(),
                approval_id,
                ApprovalDecision {
                    decision: ApprovalChoice::Once,
                    reason: None,
                },
            )
            .await
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let run = service.get_run(run_id.clone()).await.unwrap();
                if run.status.is_terminal() {
                    break run;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("approved browser download Run should terminate");
        assert_eq!(completed.status, RunStatus::Completed);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
        let projection = provider
            .download_projection
            .lock()
            .unwrap()
            .clone()
            .expect("provider should receive the safe download projection");
        let projection: JsonValue = serde_json::from_str(&projection).unwrap();
        assert_eq!(projection["download"]["name"], "report.txt");
        assert_eq!(projection["download"]["mimeType"], "text/plain");
        assert_eq!(projection["download"]["sizeBytes"], 17);
        assert_eq!(projection["download"]["scan"]["contentExposed"], false);
        assert_eq!(projection["download"]["scan"]["workspaceImported"], false);
        let projection_text = projection.to_string();
        assert!(!projection_text.contains("download fixture"));
        assert!(!projection_text.contains(home.path().to_string_lossy().as_ref()));

        let events = sessions.run_event_batch(&run_id, 0).unwrap();
        let public_events = events
            .events
            .iter()
            .map(|event| event.envelope_json.as_str())
            .collect::<String>();
        assert!(public_events.contains("browser_download"));
        assert!(
            events
                .events
                .iter()
                .any(|event| event.event_name == "approval.required")
        );
        assert!(
            events
                .events
                .iter()
                .any(|event| event.event_name == "approval.resolved")
        );
        assert!(!public_events.contains("download fixture"));
        assert!(!public_events.contains(home.path().to_string_lossy().as_ref()));
        tokio::time::timeout(Duration::from_secs(5), async {
            while browser_download_file_count(home.path()) != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("terminal Run cleanup should remove private download content");

        let _ = shutdown.send(());
        browser_server.await.unwrap();
    }

    #[test]
    fn model_preparation_injects_browser_tools_only_when_runtime_is_ready() {
        let home = tempfile::tempdir().unwrap();
        let profiles = ProfileService::without_credential_store(home.path().to_owned());
        let sessions = SessionService::new(home.path(), TOKEN);
        let profiles_handle = Arc::new(profiles.clone());
        let memory = MemoryService::new(profiles_handle.clone(), TOKEN);
        let web = WebService::new(profiles_handle)
            .unwrap_or_else(|_| WebService::unavailable(Arc::new(profiles.clone())));
        let config = profiles.get_config("default").unwrap();
        profiles
            .update_config(
                "default",
                &config.etag,
                &serde_json::json!({
                    "model": {
                        "provider": "lmstudio",
                        "model": "test-model",
                        "baseUrl": "http://127.0.0.1:1234/v1"
                    },
                    "toolsets": {"browser": true}
                }),
            )
            .unwrap();
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Browser tool injection".to_owned()),
                },
                "browser-tool-injection-session",
            )
            .unwrap();
        let request = CreateRun {
            client_request_id: "browser-tool-injection-request".to_owned(),
            message: crate::runs::ChatInput {
                text: "Inspect this page".to_owned(),
                file_ids: Vec::new(),
            },
            model_override: None,
            reasoning_effort: None,
            workspace_id: None,
        };
        let tools = ToolRegistry::hermes_v0182();
        let unavailable = prepare_model(
            ModelPreparationDependencies {
                profiles: &profiles,
                sessions: &sessions,
                memory: &memory,
                web: &web,
                tools: &tools,
                browser_available: false,
            },
            &session.value.id,
            &request,
        )
        .unwrap();
        assert!(
            unavailable
                .tools
                .iter()
                .all(|definition| !definition.name.starts_with("browser_"))
        );

        let available = prepare_model(
            ModelPreparationDependencies {
                profiles: &profiles,
                sessions: &sessions,
                memory: &memory,
                web: &web,
                tools: &tools,
                browser_available: true,
            },
            &session.value.id,
            &request,
        )
        .unwrap();
        assert_eq!(
            available
                .tools
                .iter()
                .filter(|definition| definition.name.starts_with("browser_"))
                .count(),
            13
        );
    }

    #[tokio::test]
    async fn notification_slots_exist_only_for_live_subscribers() {
        let home = tempfile::tempdir().unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let skills = Arc::new(SkillService::new(profiles.clone(), TOKEN));
        let memory = Arc::new(MemoryService::new(profiles.clone(), TOKEN));
        let web = Arc::new(
            WebService::new(profiles.clone())
                .unwrap_or_else(|_| WebService::unavailable(profiles.clone())),
        );
        let service = RunService::new(profiles, sessions, skills, memory, web);

        service.notify("run_without_subscriber");
        assert!(service.inner.notifications.lock().unwrap().is_empty());

        let mut receiver = service.subscribe("run_active");
        let mut second_receiver = service.subscribe("run_active");
        assert_eq!(service.inner.notifications.lock().unwrap().len(), 1);
        service.notify("run_active");
        assert_eq!(receiver.try_recv(), Ok(()));
        assert_eq!(second_receiver.try_recv(), Ok(()));
        service.close_notifications("run_active");
        assert_eq!(receiver.try_recv(), Ok(()));
        assert_eq!(second_receiver.try_recv(), Ok(()));
        assert!(service.inner.notifications.lock().unwrap().is_empty());

        let _missing_receiver = service.subscribe("run_missing");
        assert_eq!(service.inner.notifications.lock().unwrap().len(), 1);
        assert!(matches!(
            service.event_batch("run_missing".to_owned(), 0).await,
            Err(RunError::NotFound)
        ));
        assert!(service.inner.notifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn terminal_notifications_wait_for_async_tool_delivery_settlement() {
        let home = tempfile::tempdir().unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let skills = Arc::new(SkillService::new(profiles.clone(), TOKEN));
        let memory = Arc::new(MemoryService::new(profiles.clone(), TOKEN));
        let web = Arc::new(
            WebService::new(profiles.clone())
                .unwrap_or_else(|_| WebService::unavailable(profiles.clone())),
        );
        let service = RunService::new(profiles, sessions.clone(), skills, memory, web);
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Async delivery notification lifetime".to_owned()),
                },
                "async-delivery-notification-session",
            )
            .unwrap();
        let workspace_path = home.path().join("async-delivery-workspace");
        std::fs::create_dir_all(&workspace_path).unwrap();
        let workspace = sessions
            .register_workspace(
                "default",
                workspace_path.to_str().unwrap(),
                "async-delivery-notification-workspace",
            )
            .unwrap();
        let accepted = sessions
            .create_run(
                &session.value.id,
                &CreateRun {
                    client_request_id: "async-delivery-notification-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "start a background command".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: Some(workspace.id.clone()),
                },
                "async-delivery-notification-run",
                "test/model",
            )
            .unwrap();
        let run_id = accepted.run.id;
        let message_id = "message_async_delivery_notification";
        let call_id = "call_async_delivery_notification";
        sessions
            .begin_assistant_message(&run_id, message_id)
            .unwrap();
        sessions
            .record_provider_turn(
                &run_id,
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
                        arguments_json: r#"{"command":"cargo test"}"#.to_owned(),
                    }],
                },
            )
            .unwrap();
        let invocation = sessions
            .start_tool_invocation_with_event(
                &run_id,
                call_id,
                "terminal",
                "Run background command",
            )
            .unwrap();
        let owner = ProcessOwner {
            profile_id: "default".to_owned(),
            session_id: session.value.id,
        };
        let process_id = "process_00000000000000000000000000000001";
        sessions
            .create_process(
                &owner,
                &CreateProcess {
                    process_id: process_id.to_owned(),
                    workspace_id: workspace.id,
                    creator_run_id: run_id.clone(),
                    call_id: call_id.to_owned(),
                    command_preview: "cargo test".to_owned(),
                    command_sha256: format!("{:x}", Sha256::digest(b"cargo test")),
                    detached: true,
                    completion_notification_required: true,
                    async_delivery: Some(AsyncToolDeliveryRequest {
                        kind: AsyncToolDeliveryKind::Completion,
                        watch_patterns: Vec::new(),
                    }),
                },
            )
            .unwrap();
        sessions
            .complete_tool_invocation_with_event_and_async_delivery(
                &run_id,
                call_id,
                invocation.checkpoint,
                r#"{"ok":true}"#,
                r#"{"ok":true}"#,
                "Background command started",
                true,
            )
            .unwrap();

        let mut receiver = service.subscribe(&run_id);
        sessions
            .complete_run(
                &run_id,
                CompleteRunPlan {
                    message_id: message_id.to_owned(),
                    text: "The background command is running.".to_owned(),
                    reasoning: None,
                    tool_calls: Vec::new(),
                    usage: Usage {
                        prompt_tokens: 2,
                        completion_tokens: 2,
                        total_tokens: 4,
                        cost: None,
                    },
                    model_label: "test/model".to_owned(),
                },
            )
            .unwrap();
        service.notify_terminal_run(&run_id).await;

        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(
            service
                .inner
                .notifications
                .lock()
                .unwrap()
                .contains_key(&run_id)
        );
        assert!(!sessions.run_event_batch(&run_id, 0).unwrap().terminal);

        sessions
            .transition_process(
                &owner,
                process_id,
                ProcessStatus::Starting,
                &ProcessTransition::Running {
                    pid: 7,
                    process_identity: Some("test:7".to_owned()),
                },
            )
            .unwrap();
        sessions
            .transition_process(
                &owner,
                process_id,
                ProcessStatus::Running,
                &ProcessTransition::Exited {
                    exit_code: 0,
                    completion_reason: "command completed".to_owned(),
                    termination_source: "test".to_owned(),
                },
            )
            .unwrap();
        let delivery = sessions
            .pending_async_tool_deliveries()
            .unwrap()
            .into_iter()
            .find(|delivery| delivery.process.process_id == process_id)
            .unwrap();
        assert_eq!(
            sessions
                .settle_async_tool_delivery(&delivery, AsyncToolDeliveryTrigger::Completion)
                .unwrap(),
            AsyncToolDeliveryDisposition::Published
        );
        service.notify_terminal_run(&run_id).await;

        assert_eq!(receiver.try_recv(), Ok(()));
        assert_eq!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        );
        assert!(service.inner.notifications.lock().unwrap().is_empty());
        let batch = sessions.run_event_batch(&run_id, 0).unwrap();
        assert!(batch.terminal);
        assert_eq!(
            batch
                .events
                .iter()
                .filter(|event| event.event_name == "tool.delivery")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_cancellation_wins_over_a_late_execution_failure() {
        let home = tempfile::tempdir().unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let skills = Arc::new(SkillService::new(profiles.clone(), TOKEN));
        let memory = Arc::new(MemoryService::new(profiles.clone(), TOKEN));
        let web = Arc::new(
            WebService::new(profiles.clone())
                .unwrap_or_else(|_| WebService::unavailable(profiles.clone())),
        );
        let service = RunService::new(profiles, sessions.clone(), skills, memory, web);
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Cancellation priority".to_owned()),
                },
                "cancellation-priority-session",
            )
            .unwrap();
        let accepted = sessions
            .create_run(
                &session.value.id,
                &CreateRun {
                    client_request_id: "cancellation-priority-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "cancel this run".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                "cancellation-priority-run",
                "test/model",
            )
            .unwrap();
        let run_id = accepted.run.id;

        let (cancelling, disposition) = sessions.request_run_cancel(&run_id).unwrap();
        assert_eq!(cancelling.status, RunStatus::Cancelling);
        assert_eq!(disposition, CancelDisposition::SignalExecutor);

        service.fail_local(&run_id).await;

        let cancelled = sessions.get_run(&run_id).unwrap();
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        assert_eq!(cancelled.error, None);
        let events = sessions.run_event_batch(&run_id, 0).unwrap();
        assert!(events.terminal);
        assert_eq!(
            events
                .events
                .iter()
                .map(|event| event.event_name.as_str())
                .collect::<Vec<_>>(),
            ["run.started", "run.cancelled"]
        );
    }

    #[tokio::test]
    async fn shutdown_drains_a_never_responding_provider_before_releasing_the_lease() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Drain a stuck provider".to_owned()),
                },
                "drain-stuck-provider-session",
            )
            .unwrap();
        let (provider, mut entered, mut dropped) = NeverRespondingProvider::new();
        let service = service_with_provider(home.path(), sessions.clone(), provider.clone());
        let accepted = service
            .create_run(
                session.value.id,
                CreateRun {
                    client_request_id: "drain-stuck-provider-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "wait forever".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                "drain-stuck-provider-run".to_owned(),
            )
            .await
            .unwrap();
        wait_for_true(&mut entered).await;

        tokio::time::timeout(Duration::from_secs(2), service.shutdown())
            .await
            .expect("shutdown should abort a provider that ignores cancellation");
        wait_for_true(&mut dropped).await;

        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(service.inner.tasks.active_admission_count(), 0);
        assert_eq!(service.inner.tasks.active_task_count(), 0);
        let cancelled = sessions.get_run(&accepted.run.id).unwrap();
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        assert_eq!(cancelled.pending_action, None);
        let events = sessions.run_event_batch(&accepted.run.id, 0).unwrap();
        assert!(events.terminal);
        assert_eq!(
            events.events.last().map(|event| event.event_name.as_str()),
            Some("run.cancelled")
        );

        let successor = SessionService::new(home.path(), TOKEN);
        let lease = successor
            .acquire_runtime_lease("runtime_33333333333333333333333333333333")
            .unwrap();
        successor.release_runtime_lease(&lease);
    }

    #[tokio::test]
    async fn shutdown_preserves_runs_until_a_new_runtime_recovers_and_advances_the_queue() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Preserve a stuck provider".to_owned()),
                },
                "preserve-stuck-provider-session",
            )
            .unwrap();
        let session_id = session.value.id;
        let (provider, mut entered, mut dropped) = NeverRespondingProvider::new();
        let service = service_with_provider(home.path(), sessions.clone(), provider.clone());
        let running = service
            .create_run(
                session_id.clone(),
                CreateRun {
                    client_request_id: "preserve-running-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "remain recoverable".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                "preserve-running-run".to_owned(),
            )
            .await
            .unwrap();
        wait_for_true(&mut entered).await;
        let queued = service
            .create_run(
                session_id,
                CreateRun {
                    client_request_id: "preserve-queued-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "continue after restart".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                "preserve-queued-run".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(queued.run.status, RunStatus::Queued);

        tokio::time::timeout(Duration::from_secs(2), service.shutdown_preserving_runs())
            .await
            .expect("preserving shutdown should abort local execution promptly");
        wait_for_true(&mut dropped).await;
        assert_eq!(service.inner.tasks.active_admission_count(), 0);
        assert_eq!(service.inner.tasks.active_task_count(), 0);
        assert_eq!(
            sessions.get_run(&running.run.id).unwrap().status,
            RunStatus::Running
        );
        assert_eq!(
            sessions.get_run(&queued.run.id).unwrap().status,
            RunStatus::Queued
        );
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 1);

        let successor_sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let successor_provider = Arc::new(ImmediateProvider {
            calls: AtomicUsize::new(0),
        });
        let successor = service_with_provider(
            home.path(),
            successor_sessions.clone(),
            successor_provider.clone(),
        );
        assert_eq!(
            successor_sessions.get_run(&running.run.id).unwrap().status,
            RunStatus::Failed
        );
        let completed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let current = successor_sessions.get_run(&queued.run.id).unwrap();
                if current.status.is_terminal() {
                    return current;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the successor runtime should advance the preserved queue");
        assert_eq!(completed.status, RunStatus::Completed);
        assert_eq!(successor_provider.calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 1);
        successor.shutdown().await;
    }

    #[tokio::test]
    async fn failed_startup_recovery_releases_the_runtime_lease_and_stays_unavailable() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionService::new(home.path(), TOKEN));
        let session = sessions
            .create_session(
                &crate::sessions::CreateSession {
                    profile_id: "default".to_owned(),
                    title: Some("Invalid recovery state".to_owned()),
                },
                "invalid-recovery-session",
            )
            .unwrap();
        let accepted = sessions
            .create_run(
                &session.value.id,
                &CreateRun {
                    client_request_id: "invalid-recovery-request".to_owned(),
                    message: crate::runs::ChatInput {
                        text: "force recovery to fail".to_owned(),
                        file_ids: Vec::new(),
                    },
                    model_override: None,
                    reasoning_effort: None,
                    workspace_id: None,
                },
                "invalid-recovery-run",
                "test/model",
            )
            .unwrap();
        let connection =
            rusqlite::Connection::open(home.path().join(".synthchat/sessions-v1.db")).unwrap();
        assert_eq!(
            connection
                .execute(
                    "UPDATE runs SET status = 'waitingApproval' WHERE id = ?1",
                    [&accepted.run.id],
                )
                .unwrap(),
            1
        );

        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let skills = Arc::new(SkillService::new(profiles.clone(), TOKEN));
        let memory = Arc::new(MemoryService::new(profiles.clone(), TOKEN));
        let web = Arc::new(
            WebService::new(profiles.clone())
                .unwrap_or_else(|_| WebService::unavailable(profiles.clone())),
        );
        let service = RunService::new(profiles, sessions.clone(), skills, memory, web);

        assert!(!service.is_available());
        let expires_at: i64 = connection
            .query_row(
                "SELECT expires_at_unix_ms FROM runtime_leases WHERE lease_name = 'run-runtime'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(expires_at, 0);
        let takeover = sessions
            .acquire_runtime_lease("runtime_33333333333333333333333333333333")
            .unwrap();
        assert!(takeover.epoch >= 2);
        sessions.release_runtime_lease(&takeover);
    }
}
