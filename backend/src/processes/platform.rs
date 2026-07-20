use std::io;

use tokio::process::{Child, Command};

#[cfg(target_os = "windows")]
pub(crate) struct ProcessLifetime {
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(target_os = "windows")]
struct DescendantProcess {
    pid: u32,
    identity: String,
    handle: windows_sys::Win32::Foundation::HANDLE,
    can_terminate: bool,
}

// Windows job handles may be closed from any thread, and this type is the
// unique owner of the handle.
#[cfg(target_os = "windows")]
unsafe impl Send for ProcessLifetime {}

// Job APIs are thread-safe and this type remains the unique owner of the
// handle, so shared access for termination is safe.
#[cfg(target_os = "windows")]
unsafe impl Sync for ProcessLifetime {}

// The handle remains bound to one process object and Windows permits waiting
// and termination from any thread.
#[cfg(target_os = "windows")]
unsafe impl Send for DescendantProcess {}

#[cfg(target_os = "windows")]
unsafe impl Sync for DescendantProcess {}

#[cfg(not(target_os = "windows"))]
pub(crate) struct ProcessLifetime;

#[cfg(target_os = "windows")]
impl Drop for ProcessLifetime {
    fn drop(&mut self) {
        if !self.job.is_null() {
            // Closing the last job handle enforces KILL_ON_JOB_CLOSE.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.job);
            }
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for DescendantProcess {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn configure_spawn(command: &mut Command, _foreground: bool) -> io::Result<()> {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt;

    command
        .as_std_mut()
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    Ok(())
}

#[cfg(unix)]
pub(crate) fn configure_spawn(command: &mut Command, foreground: bool) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    command.as_std_mut().process_group(0);
    #[cfg(target_os = "linux")]
    if foreground {
        // The child must not survive an abrupt backend exit. The parent check
        // closes the fork-to-prctl race.
        unsafe {
            command.as_std_mut().pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "backend exited while spawning terminal command",
                    ));
                }
                Ok(())
            });
        }
    }
    #[cfg(target_os = "macos")]
    let _ = foreground;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn process_lifetime(pid: u32) -> io::Result<ProcessLifetime> {
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        },
    };

    let job = unsafe {
        windows_sys::Win32::System::JobObjects::CreateJobObjectW(ptr::null(), ptr::null())
    };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured == 0 {
        unsafe { CloseHandle(job) };
        return Err(io::Error::last_os_error());
    }

    let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
    if process.is_null() || process == INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(job) };
        return Err(io::Error::last_os_error());
    }
    let assigned = unsafe { AssignProcessToJobObject(job, process) };
    unsafe { CloseHandle(process) };
    if assigned == 0 {
        unsafe { CloseHandle(job) };
        return Err(io::Error::last_os_error());
    }
    Ok(ProcessLifetime { job })
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn process_lifetime(_pid: u32) -> io::Result<ProcessLifetime> {
    Ok(ProcessLifetime)
}

#[cfg(target_os = "windows")]
pub(crate) async fn terminate_tree(
    pid: u32,
    child: &mut Child,
    lifetime: &ProcessLifetime,
) -> io::Result<()> {
    let descendants = descendant_processes(pid);
    // Terminate the owned job first so a guardian blocked forwarding stdin
    // cannot consume the control deadline. Handles captured before termination
    // retain authority over shells that escaped the job without relying on PID
    // lookup or a potentially blocking external helper subprocess.
    let job_termination = terminate_owned_job(lifetime);
    let job_termination_failed = job_termination.is_err();
    let mut cleanup_error = job_termination.err();
    let descendants = match descendants {
        Ok(descendants) => descendants,
        Err(error) => {
            if job_termination_failed {
                cleanup_error.get_or_insert(error);
            } else {
                tracing::warn!(
                    pid,
                    ?error,
                    "ignored non-authoritative descendant discovery failure after Job termination"
                );
            }
            Vec::new()
        }
    };
    if job_termination_failed && let Err(error) = child.start_kill() {
        cleanup_error.get_or_insert(error);
    }
    if let Err(error) = child.wait().await {
        cleanup_error.get_or_insert(error);
    }
    for descendant in &descendants {
        if let Err(error) = settle_descendant(descendant).await {
            if job_termination_failed {
                cleanup_error.get_or_insert(error);
            } else {
                tracing::warn!(
                    pid,
                    descendant_pid = descendant.pid,
                    ?error,
                    "ignored non-authoritative descendant cleanup failure after Job termination"
                );
            }
        }
    }
    cleanup_error.map_or(Ok(()), Err)
}

#[cfg(target_os = "windows")]
fn terminate_owned_job(lifetime: &ProcessLifetime) -> io::Result<()> {
    let terminated =
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(lifetime.job, 1) };
    if terminated == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn terminate_lifetime_now(lifetime: &ProcessLifetime) -> io::Result<()> {
    // A Job handle is bound to the exact process containment boundary created
    // for this launch. This is safe to call from Drop and never re-resolves a
    // PID or waits on a helper process.
    terminate_owned_job(lifetime)
}

#[cfg(target_os = "windows")]
fn descendant_processes(root_pid: u32) -> io::Result<Vec<DescendantProcess>> {
    use std::{collections::HashSet, mem::size_of};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        let error = io::Error::last_os_error();
        unsafe { CloseHandle(snapshot) };
        return Err(error);
    }
    let mut processes = Vec::new();
    loop {
        processes.push((entry.th32ProcessID, entry.th32ParentProcessID));
        if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            const ERROR_NO_MORE_FILES: i32 = 18;
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES) {
                unsafe { CloseHandle(snapshot) };
                return Err(error);
            }
            break;
        }
    }
    unsafe { CloseHandle(snapshot) };

    let mut tree = HashSet::from([root_pid]);
    loop {
        let before = tree.len();
        for &(process_id, parent_id) in &processes {
            if process_id != 0 && tree.contains(&parent_id) {
                tree.insert(process_id);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    tree.remove(&root_pid);
    let mut descendants = Vec::with_capacity(tree.len());
    for process_id in tree {
        if let Some(process) = open_descendant_process(process_id)? {
            descendants.push(process);
        }
    }
    Ok(descendants)
}

#[cfg(target_os = "windows")]
fn open_descendant_process(pid: u32) -> io::Result<Option<DescendantProcess>> {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        },
    };

    let mut can_terminate = true;
    let mut handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        const ERROR_INVALID_PARAMETER: i32 = 87;
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            return Ok(None);
        }
        const ERROR_ACCESS_DENIED: i32 = 5;
        if error.raw_os_error() != Some(ERROR_ACCESS_DENIED) {
            return Err(error);
        }
        can_terminate = false;
        handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            let retry_error = io::Error::last_os_error();
            return if retry_error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
                Ok(None)
            } else {
                Err(io::Error::new(
                    retry_error.kind(),
                    format!("failed to retain descendant process {pid}: {retry_error}"),
                ))
            };
        }
    }
    match process_handle_exited(handle) {
        Ok(true) => {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Ok(None);
        }
        Ok(false) => {}
        Err(error) => {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(error);
        }
    }
    let identity = match process_identity_from_handle(handle) {
        Ok(identity) => identity,
        Err(error) => {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(error);
        }
    };
    Ok(Some(DescendantProcess {
        pid,
        identity,
        handle,
        can_terminate,
    }))
}

#[cfg(target_os = "windows")]
fn retained_process_for_identity(
    pid: u32,
    expected_identity: &str,
) -> io::Result<DescendantProcess> {
    let process = open_descendant_process(pid)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "process is no longer available"))?;
    if process.identity != expected_identity {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process identity changed",
        ));
    }
    Ok(process)
}

#[cfg(target_os = "windows")]
fn terminate_descendant_now(process: &DescendantProcess) -> io::Result<()> {
    if process_handle_exited(process.handle)? {
        return Ok(());
    }
    if !process.can_terminate {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "descendant process {} ({}) could not be opened for termination",
                process.pid, process.identity
            ),
        ));
    }
    let terminated =
        unsafe { windows_sys::Win32::System::Threading::TerminateProcess(process.handle, 1) };
    if terminated == 0 {
        let error = io::Error::last_os_error();
        if process_handle_exited(process.handle)? {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
async fn settle_descendant(process: &DescendantProcess) -> io::Result<()> {
    if wait_for_descendant_exit(process, 4).await? {
        return Ok(());
    }
    let termination_error = terminate_descendant_now(process).err();
    if wait_for_descendant_exit(process, 80).await? {
        return Ok(());
    }
    if let Some(error) = termination_error {
        return Err(error);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "process {} ({}) did not exit after termination",
            process.pid, process.identity
        ),
    ))
}

#[cfg(target_os = "windows")]
async fn wait_for_descendant_exit(
    process: &DescendantProcess,
    attempts: usize,
) -> io::Result<bool> {
    for _ in 0..attempts {
        if process_handle_exited(process.handle)? {
            return Ok(true);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    process_handle_exited(process.handle)
}

#[cfg(target_os = "windows")]
fn process_handle_exited(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<bool> {
    use windows_sys::Win32::{
        Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };

    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        status => Err(io::Error::other(format!(
            "unexpected process wait status {status:#x}"
        ))),
    }
}

#[cfg(target_os = "windows")]
fn process_identity_from_handle(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<String> {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let succeeded =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Ok(format!("windows-filetime:{ticks}"))
}

#[cfg(unix)]
pub(crate) async fn terminate_tree(
    pid: u32,
    child: &mut Child,
    _lifetime: &ProcessLifetime,
) -> io::Result<()> {
    let process_group = i32::try_from(pid)
        .ok()
        .and_then(i32::checked_neg)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid process id"))?;
    let term = unsafe { libc::kill(process_group, libc::SIGTERM) };
    if term == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let kill = unsafe { libc::kill(process_group, libc::SIGKILL) };
    if kill == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    let _ = child.wait().await?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };

    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() || process == INVALID_HANDLE_VALUE {
        return None;
    }
    if unsafe { WaitForSingleObject(process, 0) } != WAIT_TIMEOUT {
        unsafe { CloseHandle(process) };
        return None;
    }
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let succeeded =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(process) };
    if succeeded == 0 {
        return None;
    }
    let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Some(format!("windows-filetime:{ticks}"))
}

#[cfg(target_os = "linux")]
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(") ")?.1;
    let start_ticks = after_name.split_whitespace().nth(19)?;
    Some(format!("linux:{}:{start_ticks}", boot_id.trim()))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok()?;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let read =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if read != size || info.pbi_pid != u32::try_from(pid).ok()? {
        return None;
    }
    Some(format!(
        "macos-start:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

pub(crate) fn identity_matches(pid: u32, expected: &str) -> bool {
    process_identity(pid).as_deref() == Some(expected)
}

#[cfg(target_os = "windows")]
pub(crate) fn terminate_tree_now(pid: u32, expected_identity: &str) -> bool {
    // Drop paths cannot await descendant cleanup. The guardian owns a Job for
    // its target, so terminating this retained root handle closes that Job and
    // tears down the nested tree without a PID lookup after validation.
    let Ok(process) = retained_process_for_identity(pid, expected_identity) else {
        return false;
    };
    terminate_descendant_now(&process).is_ok()
}

#[cfg(unix)]
pub(crate) fn terminate_tree_now(pid: u32, expected_identity: &str) -> bool {
    if !identity_matches(pid, expected_identity) {
        return false;
    }
    let Some(group) = i32::try_from(pid).ok().and_then(i32::checked_neg) else {
        return false;
    };
    let result = unsafe { libc::kill(group, libc::SIGKILL) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "windows")]
pub(crate) fn finish_after_root_exit(_pid: u32, lifetime: ProcessLifetime) -> io::Result<()> {
    // KILL_ON_JOB_CLOSE eventually tears down members, but pipe-owning
    // descendants can keep the parent's drains open while that close is
    // being processed. Terminate the owned job first so foreground cleanup is
    // bounded before releasing its final handle.
    let result = terminate_owned_job(&lifetime);
    drop(lifetime);
    result
}

#[cfg(unix)]
pub(crate) fn finish_after_root_exit(pid: u32, lifetime: ProcessLifetime) -> io::Result<()> {
    let group = i32::try_from(pid)
        .ok()
        .and_then(i32::checked_neg)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid process id"))?;
    let result = unsafe { libc::kill(group, libc::SIGKILL) };
    drop(lifetime);
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) async fn terminate_detached_tree(pid: u32, expected_identity: &str) -> io::Result<()> {
    let process = retained_process_for_identity(pid, expected_identity)?;
    terminate_descendant_now(&process)?;
    if wait_for_descendant_exit(&process, 80).await? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "process remained alive after tree termination",
    ))
}

#[cfg(unix)]
pub(crate) async fn terminate_detached_tree(pid: u32, expected_identity: &str) -> io::Result<()> {
    if !identity_matches(pid, expected_identity) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process identity changed",
        ));
    }
    let group = i32::try_from(pid)
        .ok()
        .and_then(i32::checked_neg)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid process id"))?;
    let term = unsafe { libc::kill(group, libc::SIGTERM) };
    if term == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let kill = unsafe { libc::kill(group, libc::SIGKILL) };
    if kill == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    wait_for_identity_exit(pid, expected_identity).await
}

#[cfg(unix)]
async fn wait_for_identity_exit(pid: u32, expected_identity: &str) -> io::Result<()> {
    for _ in 0..40 {
        if !identity_matches(pid, expected_identity) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "process remained alive after tree termination",
    ))
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use std::os::windows::process::CommandExt;

    use super::*;

    #[test]
    fn exited_process_with_still_active_exit_code_is_not_live() {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/D", "/S", "/C", "ping -n 2 127.0.0.1 >NUL & exit /B 259"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let pid = child.id();
        let identity = process_identity(pid).expect("the test process must start alive");
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(259));
        assert!(!identity_matches(pid, &identity));
    }

    #[test]
    fn retained_handle_termination_rejects_mismatched_identity() {
        let mut child = spawn_long_running_process();
        let pid = child.id();
        let identity = process_identity(pid).expect("the test process must start alive");

        assert!(!terminate_tree_now(pid, "windows-filetime:0"));
        assert!(identity_matches(pid, &identity));
        assert!(terminate_tree_now(pid, &identity));
        let _ = child.wait().unwrap();
        assert!(!identity_matches(pid, &identity));
    }

    #[tokio::test]
    async fn detached_termination_uses_the_verified_handle() {
        let mut child = spawn_long_running_process();
        let pid = child.id();
        let identity = process_identity(pid).expect("the test process must start alive");

        let error = terminate_detached_tree(pid, "windows-filetime:0")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(identity_matches(pid, &identity));

        terminate_detached_tree(pid, &identity).await.unwrap();
        let _ = child.wait().unwrap();
        assert!(!identity_matches(pid, &identity));
    }

    fn spawn_long_running_process() -> std::process::Child {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "while ($true) { [Threading.Thread]::Sleep(1000) }",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }
}
