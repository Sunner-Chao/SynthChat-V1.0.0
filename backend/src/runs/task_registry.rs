use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU8, Ordering},
    },
};

use tokio::{
    sync::{oneshot, watch},
    task::AbortHandle,
    time::Instant,
};
use uuid::Uuid;

use crate::tools::ToolExecutionControl;

const STOP_NONE: u8 = 0;
const STOP_PRESERVE: u8 = 1;
const STOP_DRAIN: u8 = 2;
const STOP_USER_CANCEL: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StopReason {
    Preserve,
    Drain,
    UserCancel,
}

impl StopReason {
    fn code(self) -> u8 {
        match self {
            Self::Preserve => STOP_PRESERVE,
            Self::Drain => STOP_DRAIN,
            Self::UserCancel => STOP_USER_CANCEL,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            STOP_PRESERVE => Some(Self::Preserve),
            STOP_DRAIN => Some(Self::Drain),
            STOP_USER_CANCEL => Some(Self::UserCancel),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(super) struct StopControl {
    inner: Arc<StopControlInner>,
}

struct StopControlInner {
    reason: AtomicU8,
    stopped: watch::Sender<bool>,
    tool_control: ToolExecutionControl,
}

impl StopControl {
    pub(super) fn new(tool_control: ToolExecutionControl) -> Self {
        let (stopped, _) = watch::channel(false);
        Self {
            inner: Arc::new(StopControlInner {
                reason: AtomicU8::new(STOP_NONE),
                stopped,
                tool_control,
            }),
        }
    }

    pub(super) fn request(&self, reason: StopReason) -> StopReason {
        let requested = reason.code();
        let mut current = self.inner.reason.load(Ordering::Acquire);
        while current < requested {
            match self.inner.reason.compare_exchange_weak(
                current,
                requested,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    current = requested;
                    break;
                }
                Err(observed) => current = observed,
            }
        }
        self.inner.tool_control.cancel();
        let _ = self.inner.stopped.send_replace(true);
        StopReason::from_code(current.max(requested))
            .expect("a requested stop always has a valid reason")
    }

    pub(super) fn reason(&self) -> Option<StopReason> {
        StopReason::from_code(self.inner.reason.load(Ordering::Acquire))
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<bool> {
        self.inner.stopped.subscribe()
    }

    pub(super) fn tool_control(&self) -> ToolExecutionControl {
        self.inner.tool_control.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShutdownMode {
    Drain,
    PreserveRuns,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RuntimePhase {
    Accepting,
    Stopping { token: Uuid, mode: ShutdownMode },
    Stopped { token: Uuid, mode: ShutdownMode },
}

impl RuntimePhase {
    fn shutdown_mode(&self) -> Option<ShutdownMode> {
        match self {
            Self::Accepting => None,
            Self::Stopping { mode, .. } | Self::Stopped { mode, .. } => Some(*mode),
        }
    }
}

#[derive(Clone)]
pub(super) struct TrackedRun {
    run_id: String,
    stop: StopControl,
}

impl TrackedRun {
    pub(super) fn new(run_id: impl Into<String>, stop: StopControl) -> Self {
        Self {
            run_id: run_id.into(),
            stop,
        }
    }

    pub(super) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(super) fn stop(&self) -> &StopControl {
        &self.stop
    }
}

struct TrackedTask {
    abort: AbortHandle,
    run: Option<TrackedRun>,
}

struct RegistryState {
    phase: RuntimePhase,
    admissions: HashSet<Uuid>,
    tasks: HashMap<Uuid, TrackedTask>,
    by_run: HashMap<String, Uuid>,
}

pub(super) struct TrackedTaskRegistry {
    registry_id: Uuid,
    state: Mutex<RegistryState>,
    revision: watch::Sender<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegistryError {
    ShuttingDown(ShutdownMode),
    InvalidAdmission,
    DuplicateRun,
    TaskUnavailable,
}

pub(super) struct AdmissionGuard {
    registry_id: Uuid,
    admission_id: Uuid,
    registry: Weak<TrackedTaskRegistry>,
    active: bool,
}

impl AdmissionGuard {
    pub(super) fn release(mut self) {
        self.unregister();
    }

    fn unregister(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let removed = registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admissions
            .remove(&self.admission_id);
        if removed {
            registry.bump_revision();
        }
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        self.unregister();
    }
}

struct TaskGuard {
    registry: Weak<TrackedTaskRegistry>,
    task_id: Uuid,
    run_id: Option<String>,
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.complete_task(self.task_id, self.run_id.as_deref());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShutdownTicket {
    token: Uuid,
    mode: ShutdownMode,
    leader: bool,
}

impl ShutdownTicket {
    pub(super) fn mode(&self) -> ShutdownMode {
        self.mode
    }

    pub(super) fn is_leader(&self) -> bool {
        self.leader
    }
}

impl TrackedTaskRegistry {
    pub(super) fn new() -> Self {
        let (revision, _) = watch::channel(0);
        Self {
            registry_id: Uuid::new_v4(),
            state: Mutex::new(RegistryState {
                phase: RuntimePhase::Accepting,
                admissions: HashSet::new(),
                tasks: HashMap::new(),
                by_run: HashMap::new(),
            }),
            revision,
        }
    }

    pub(super) fn phase(&self) -> RuntimePhase {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase
            .clone()
    }

    pub(super) fn begin_admission(self: &Arc<Self>) -> Result<AdmissionGuard, RegistryError> {
        let admission_id = Uuid::new_v4();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(mode) = state.phase.shutdown_mode() {
                return Err(RegistryError::ShuttingDown(mode));
            }
            state.admissions.insert(admission_id);
        }
        self.bump_revision();
        Ok(AdmissionGuard {
            registry_id: self.registry_id,
            admission_id,
            registry: Arc::downgrade(self),
            active: true,
        })
    }

    pub(super) fn spawn_tracked<F>(
        self: &Arc<Self>,
        admission: &AdmissionGuard,
        run: Option<TrackedRun>,
        future: F,
    ) -> Result<Uuid, RegistryError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task_id = Uuid::new_v4();
        let run_id = run.as_ref().map(|run| run.run_id.clone());
        let (start_sender, start_receiver) = oneshot::channel();
        let guard = TaskGuard {
            registry: Arc::downgrade(self),
            task_id,
            run_id: run_id.clone(),
        };

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if admission.registry_id != self.registry_id
            || !admission.active
            || !state.admissions.contains(&admission.admission_id)
        {
            return Err(RegistryError::InvalidAdmission);
        }
        if let RuntimePhase::Stopped { mode, .. } = &state.phase {
            return Err(RegistryError::ShuttingDown(*mode));
        }
        if run_id
            .as_ref()
            .is_some_and(|run_id| state.by_run.contains_key(run_id))
        {
            return Err(RegistryError::DuplicateRun);
        }

        let task = tokio::spawn(async move {
            let _guard = guard;
            if start_receiver.await.is_ok() {
                future.await;
            }
        });
        let abort = task.abort_handle();
        drop(task);
        state.tasks.insert(task_id, TrackedTask { abort, run });
        if let Some(run_id) = run_id.as_ref() {
            state.by_run.insert(run_id.clone(), task_id);
        }
        if start_sender.send(()).is_err() {
            let removed = state.tasks.remove(&task_id);
            if let Some(run_id) = run_id.as_ref()
                && state.by_run.get(run_id) == Some(&task_id)
            {
                state.by_run.remove(run_id);
            }
            drop(state);
            if let Some(task) = removed {
                task.abort.abort();
            }
            self.bump_revision();
            return Err(RegistryError::TaskUnavailable);
        }
        drop(state);
        self.bump_revision();
        Ok(task_id)
    }

    pub(super) fn begin_shutdown(&self, requested: ShutdownMode) -> ShutdownTicket {
        let (ticket, changed) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.phase.clone() {
                RuntimePhase::Accepting => {
                    let token = Uuid::new_v4();
                    state.phase = RuntimePhase::Stopping {
                        token,
                        mode: requested,
                    };
                    (
                        ShutdownTicket {
                            token,
                            mode: requested,
                            leader: true,
                        },
                        true,
                    )
                }
                RuntimePhase::Stopping { token, mode } | RuntimePhase::Stopped { token, mode } => (
                    ShutdownTicket {
                        token,
                        mode,
                        leader: false,
                    },
                    false,
                ),
            }
        };
        if changed {
            self.bump_revision();
        }
        ticket
    }

    pub(super) fn finish_shutdown(&self, ticket: &ShutdownTicket) -> bool {
        if !ticket.leader {
            return false;
        }
        let changed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.phase.clone() {
                RuntimePhase::Stopping { token, mode }
                    if token == ticket.token
                        && mode == ticket.mode
                        && state.admissions.is_empty()
                        && state.tasks.is_empty() =>
                {
                    state.phase = RuntimePhase::Stopped { token, mode };
                    true
                }
                RuntimePhase::Stopped { token, mode }
                    if token == ticket.token && mode == ticket.mode =>
                {
                    return true;
                }
                _ => false,
            }
        };
        if changed {
            self.bump_revision();
        }
        changed
    }

    pub(super) fn control_for_run(&self, run_id: &str) -> Option<StopControl> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task_id = state.by_run.get(run_id)?;
        state
            .tasks
            .get(task_id)?
            .run
            .as_ref()
            .map(|run| run.stop.clone())
    }

    pub(super) fn snapshot_runs(&self) -> Vec<TrackedRun> {
        let mut runs = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .values()
            .filter_map(|task| task.run.clone())
            .collect::<Vec<_>>();
        runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        runs
    }

    #[cfg(test)]
    pub(super) fn abort_run(&self, run_id: &str) -> bool {
        let abort = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .by_run
                .get(run_id)
                .and_then(|task_id| state.tasks.get(task_id))
                .map(|task| task.abort.clone())
        };
        if let Some(abort) = abort {
            abort.abort();
            true
        } else {
            false
        }
    }

    pub(super) fn abort_all(&self) -> Vec<TrackedRun> {
        let (aborts, mut runs) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                state
                    .tasks
                    .values()
                    .map(|task| task.abort.clone())
                    .collect::<Vec<_>>(),
                state
                    .tasks
                    .values()
                    .filter_map(|task| task.run.clone())
                    .collect::<Vec<_>>(),
            )
        };
        for abort in aborts {
            abort.abort();
        }
        runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        runs
    }

    pub(super) async fn wait_admissions_empty_until(&self, deadline: Instant) -> bool {
        let mut revision = self.revision.subscribe();
        loop {
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .admissions
                .is_empty()
            {
                return true;
            }
            if tokio::time::timeout_at(deadline, revision.changed())
                .await
                .is_err()
            {
                return false;
            }
        }
    }

    pub(super) async fn wait_empty_until(&self, deadline: Instant) -> bool {
        let mut revision = self.revision.subscribe();
        loop {
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tasks
                .is_empty()
            {
                return true;
            }
            if tokio::time::timeout_at(deadline, revision.changed())
                .await
                .is_err()
            {
                return false;
            }
        }
    }

    pub(super) async fn wait_stopped_until(
        &self,
        ticket: &ShutdownTicket,
        deadline: Instant,
    ) -> bool {
        let mut revision = self.revision.subscribe();
        loop {
            if matches!(
                self.phase(),
                RuntimePhase::Stopped { token, mode }
                    if token == ticket.token && mode == ticket.mode
            ) {
                return true;
            }
            if tokio::time::timeout_at(deadline, revision.changed())
                .await
                .is_err()
            {
                return false;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn active_admission_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admissions
            .len()
    }

    #[cfg(test)]
    pub(super) fn active_task_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .len()
    }

    fn complete_task(&self, task_id: Uuid, run_id: Option<&str>) {
        let changed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed = state.tasks.remove(&task_id).is_some();
            let mut index_removed = false;
            if let Some(run_id) = run_id
                && state.by_run.get(run_id) == Some(&task_id)
            {
                state.by_run.remove(run_id);
                index_removed = true;
            }
            removed || index_removed
        };
        if changed {
            self.bump_revision();
        }
    }

    fn bump_revision(&self) {
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant as StdInstant},
    };

    use super::*;
    use crate::tools::ToolExecutionControlError;

    fn stop_control() -> StopControl {
        StopControl::new(ToolExecutionControl::new(
            StdInstant::now() + Duration::from_secs(30),
        ))
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }

    #[tokio::test]
    async fn stop_control_preserves_the_strongest_reason_and_cancels_tools() {
        let stop = stop_control();
        let mut stopped = stop.subscribe();
        assert_eq!(stop.reason(), None);

        assert_eq!(stop.request(StopReason::Preserve), StopReason::Preserve);
        stopped.changed().await.unwrap();
        assert!(*stopped.borrow());
        assert_eq!(
            stop.tool_control().check(),
            Err(ToolExecutionControlError::Cancelled)
        );

        assert_eq!(stop.request(StopReason::Drain), StopReason::Drain);
        assert_eq!(stop.request(StopReason::Preserve), StopReason::Drain);
        assert_eq!(stop.request(StopReason::UserCancel), StopReason::UserCancel);
        assert_eq!(stop.reason(), Some(StopReason::UserCancel));
    }

    #[tokio::test]
    async fn task_cannot_run_before_its_run_index_is_registered() {
        let registry = Arc::new(TrackedTaskRegistry::new());
        let admission = registry.begin_admission().unwrap();
        let (observed_sender, observed) = oneshot::channel();
        let task_registry = registry.clone();
        registry
            .spawn_tracked(
                &admission,
                Some(TrackedRun::new("run_barrier", stop_control())),
                async move {
                    let _ = observed_sender
                        .send(task_registry.control_for_run("run_barrier").is_some());
                },
            )
            .unwrap();
        admission.release();

        assert!(observed.await.unwrap());
        assert!(registry.wait_empty_until(deadline()).await);
        assert_eq!(registry.active_task_count(), 0);
    }

    #[tokio::test]
    async fn admitted_operation_can_register_after_shutdown_closes_the_gate() {
        let registry = Arc::new(TrackedTaskRegistry::new());
        let admission = registry.begin_admission().unwrap();
        let ticket = registry.begin_shutdown(ShutdownMode::Drain);
        assert!(ticket.is_leader());
        assert_eq!(ticket.mode(), ShutdownMode::Drain);
        assert_eq!(
            registry.begin_admission().err(),
            Some(RegistryError::ShuttingDown(ShutdownMode::Drain))
        );

        let (release_sender, release) = oneshot::channel();
        registry
            .spawn_tracked(
                &admission,
                Some(TrackedRun::new("run_admitted", stop_control())),
                async move {
                    let _ = release.await;
                },
            )
            .unwrap();
        assert_eq!(registry.active_admission_count(), 1);
        admission.release();
        assert!(registry.wait_admissions_empty_until(deadline()).await);
        assert!(!registry.finish_shutdown(&ticket));

        release_sender.send(()).unwrap();
        assert!(registry.wait_empty_until(deadline()).await);
        assert!(registry.finish_shutdown(&ticket));
        assert!(registry.wait_stopped_until(&ticket, deadline()).await);
    }

    #[tokio::test]
    async fn first_shutdown_mode_wins_and_followers_observe_completion() {
        let registry = Arc::new(TrackedTaskRegistry::new());
        let leader = registry.begin_shutdown(ShutdownMode::PreserveRuns);
        let follower = registry.begin_shutdown(ShutdownMode::Drain);

        assert!(leader.is_leader());
        assert!(!follower.is_leader());
        assert_eq!(follower.mode(), ShutdownMode::PreserveRuns);
        assert!(!registry.finish_shutdown(&follower));
        assert!(registry.finish_shutdown(&leader));
        assert!(registry.wait_stopped_until(&follower, deadline()).await);
    }

    #[tokio::test]
    async fn abort_drops_the_task_and_releases_its_run_index() {
        struct DropProbe(Arc<AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let registry = Arc::new(TrackedTaskRegistry::new());
        let admission = registry.begin_admission().unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let probe = DropProbe(dropped.clone());
        let (entered_sender, entered) = oneshot::channel();
        registry
            .spawn_tracked(
                &admission,
                Some(TrackedRun::new("run_abort", stop_control())),
                async move {
                    let _probe = probe;
                    let _ = entered_sender.send(());
                    future::pending::<()>().await;
                },
            )
            .unwrap();
        admission.release();
        entered.await.unwrap();

        assert!(registry.abort_run("run_abort"));
        assert!(registry.wait_empty_until(deadline()).await);
        assert!(dropped.load(Ordering::Acquire));
        assert!(registry.control_for_run("run_abort").is_none());
    }

    #[test]
    fn late_guard_cannot_remove_a_replacement_run_index() {
        let registry = TrackedTaskRegistry::new();
        let old_task = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        registry
            .state
            .lock()
            .unwrap()
            .by_run
            .insert("run_aba".to_owned(), replacement);

        registry.complete_task(old_task, Some("run_aba"));

        assert_eq!(
            registry.state.lock().unwrap().by_run.get("run_aba"),
            Some(&replacement)
        );
    }
}
