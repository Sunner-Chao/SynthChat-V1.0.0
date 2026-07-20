use std::{ffi::OsString, fmt, io, path::Path, process::ExitStatus, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin},
    task::JoinHandle,
};
use zeroize::Zeroize;

use super::{
    guardian::{
        CodeRpcBootstrap, ProcessGuardianLaunch, encode_process_guardian_launch,
        encode_process_guardian_stdin_close, process_guardian_command,
    },
    output::{CaptureMode, CapturedOutput, ProcessOutputCapture, ProcessStream},
    platform,
    shell::{guardian_backend_executable, sanitized_environment},
};

const GUARDIAN_HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const DIRECT_PROCESS_STDERR_LIMIT_BYTES: usize = 10_000;

pub(crate) struct DirectProcessRequest {
    launch: ProcessGuardianLaunch,
}

impl DirectProcessRequest {
    pub(crate) fn new(
        executable: &Path,
        cwd: &Path,
        arguments: impl IntoIterator<Item = OsString>,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
        code_rpc: Option<CodeRpcBootstrap>,
    ) -> io::Result<Self> {
        let arguments = arguments
            .into_iter()
            .map(|argument| {
                argument.into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "direct process argument contains non-Unicode data",
                    )
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let environment = environment
            .into_iter()
            .map(|(name, value)| {
                let name = name.into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "direct process environment name contains non-Unicode data",
                    )
                })?;
                let value = value.into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "direct process environment value contains non-Unicode data",
                    )
                })?;
                Ok((name, value))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let launch =
            ProcessGuardianLaunch::new_direct(executable, cwd, arguments, environment, code_rpc)
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "direct process launch configuration is invalid",
                    )
                })?;
        Ok(Self { launch })
    }
}

impl fmt::Debug for DirectProcessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectProcessRequest")
            .field("launch", &self.launch)
            .finish()
    }
}

pub(crate) struct DirectProcessOutput {
    pub(crate) stdout: CapturedOutput,
    pub(crate) stderr: CapturedOutput,
}

impl fmt::Debug for DirectProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectProcessOutput")
            .field("stdout", &"[redacted]")
            .field("stderr", &"[redacted]")
            .field("stdout_observed_bytes", &self.stdout.observed_bytes)
            .field("stderr_observed_bytes", &self.stderr.observed_bytes)
            .field("stdout_truncated", &self.stdout.truncated)
            .field("stderr_truncated", &self.stderr.truncated)
            .finish()
    }
}

/// Owns a guardian and its platform containment handle. Dropping an unsettled
/// value synchronously terminates the owned containment boundary; callers
/// should still prefer [`Self::terminate`] so pipe drains can finish cleanly.
pub(crate) struct SupervisedDirectProcess {
    pid: u32,
    #[cfg(not(target_os = "windows"))]
    identity: String,
    child: Option<Child>,
    lifetime: Option<platform::ProcessLifetime>,
    guardian_stdin: Option<ChildStdin>,
    stdout: ProcessOutputCapture,
    stderr: ProcessOutputCapture,
    drains: Vec<JoinHandle<io::Result<()>>>,
    settled: bool,
}

impl SupervisedDirectProcess {
    pub(crate) async fn spawn(request: DirectProcessRequest) -> io::Result<Self> {
        let mut launch_frame = encode_process_guardian_launch(&request.launch).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "direct process guardian frame is invalid",
            )
        })?;
        drop(request);

        let backend = guardian_backend_executable()?;
        let mut command = process_guardian_command(&backend, true)?;
        command.env_clear().envs(sanitized_environment());
        let mut child = command.spawn()?;
        let pid = match child.id() {
            Some(pid) => pid,
            None => {
                launch_frame.zeroize();
                stop_uncontained_child(&mut child).await;
                return Err(io::Error::other(
                    "direct process guardian has no process id",
                ));
            }
        };
        let lifetime = match platform::process_lifetime(pid) {
            Ok(lifetime) => lifetime,
            Err(error) => {
                launch_frame.zeroize();
                stop_uncontained_child(&mut child).await;
                return Err(io::Error::new(
                    error.kind(),
                    "direct process containment could not be established",
                ));
            }
        };
        #[cfg(target_os = "windows")]
        if platform::process_identity(pid).is_none() {
            launch_frame.zeroize();
            let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
            let _ = platform::finish_after_root_exit(pid, lifetime);
            return Err(io::Error::other(
                "direct process identity could not be established",
            ));
        }
        #[cfg(not(target_os = "windows"))]
        let identity = match platform::process_identity(pid) {
            Some(identity) => identity,
            None => {
                launch_frame.zeroize();
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                let _ = platform::finish_after_root_exit(pid, lifetime);
                return Err(io::Error::other(
                    "direct process identity could not be established",
                ));
            }
        };
        let guardian_stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                launch_frame.zeroize();
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                let _ = platform::finish_after_root_exit(pid, lifetime);
                return Err(io::Error::other(
                    "direct process guardian control pipe is unavailable",
                ));
            }
        };
        let stdout_pipe = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                launch_frame.zeroize();
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                let _ = platform::finish_after_root_exit(pid, lifetime);
                return Err(io::Error::other(
                    "direct process stdout pipe is unavailable",
                ));
            }
        };
        let stderr_pipe = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                launch_frame.zeroize();
                let _ = platform::terminate_tree(pid, &mut child, &lifetime).await;
                let _ = platform::finish_after_root_exit(pid, lifetime);
                return Err(io::Error::other(
                    "direct process stderr pipe is unavailable",
                ));
            }
        };
        let stdout = ProcessOutputCapture::new(CaptureMode::Foreground);
        let stderr = ProcessOutputCapture::new(CaptureMode::HeadOnly {
            maximum_bytes: DIRECT_PROCESS_STDERR_LIMIT_BYTES,
        });
        let drains = vec![
            spawn_drain(stdout_pipe, stdout.clone(), ProcessStream::Stdout),
            spawn_drain(stderr_pipe, stderr.clone(), ProcessStream::Stderr),
        ];
        let mut process = Self {
            pid,
            #[cfg(not(target_os = "windows"))]
            identity,
            child: Some(child),
            lifetime: Some(lifetime),
            guardian_stdin: Some(guardian_stdin),
            stdout,
            stderr,
            drains,
            settled: false,
        };

        let close_frame = encode_process_guardian_stdin_close();
        let handoff = tokio::time::timeout(GUARDIAN_HANDOFF_TIMEOUT, async {
            let stdin = process.guardian_stdin.as_mut().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "direct process guardian control pipe is unavailable",
                )
            })?;
            stdin.write_all(&launch_frame).await?;
            stdin.write_all(&close_frame).await?;
            stdin.flush().await
        })
        .await;
        launch_frame.zeroize();
        match handoff {
            Ok(Ok(())) => Ok(process),
            Ok(Err(error)) => {
                let _ = process.terminate().await;
                Err(io::Error::new(
                    error.kind(),
                    "direct process guardian handoff failed",
                ))
            }
            Err(_) => {
                let _ = process.terminate().await;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "direct process guardian handoff timed out",
                ))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.settled {
            return Err(io::Error::other("direct process is already settled"));
        }
        let status = match self.child.as_mut() {
            Some(child) => child.wait().await,
            None => Err(io::Error::other("direct process child is unavailable")),
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                let _ = self.terminate().await;
                return Err(io::Error::new(
                    error.kind(),
                    "direct process status is unavailable",
                ));
            }
        };
        self.finish_after_root_exit().await?;
        Ok(status)
    }

    pub(crate) async fn terminate(&mut self) -> io::Result<()> {
        if self.settled {
            return Ok(());
        }
        let terminated = match (self.child.as_mut(), self.lifetime.as_ref()) {
            (Some(child), Some(lifetime)) => {
                platform::terminate_tree(self.pid, child, lifetime).await
            }
            _ => Err(io::Error::other(
                "direct process containment is unavailable",
            )),
        };
        let cleanup = self.finish_after_root_exit().await;
        match (terminated, cleanup) {
            (Err(error), _) => Err(io::Error::new(
                error.kind(),
                "direct process tree termination failed",
            )),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub(crate) fn captured_output<R>(
        &self,
        redactor: &R,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> io::Result<DirectProcessOutput>
    where
        R: Fn(&str) -> String,
    {
        if !self.settled {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "direct process output is not final",
            ));
        }
        if stderr_limit != DIRECT_PROCESS_STDERR_LIMIT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "direct process stderr limit must match its head-only capture bound",
            ));
        }
        Ok(DirectProcessOutput {
            stdout: self.stdout.finish_bounded(redactor, stdout_limit),
            stderr: self.stderr.finish(redactor),
        })
    }

    async fn finish_after_root_exit(&mut self) -> io::Result<()> {
        let cleanup = self
            .lifetime
            .take()
            .ok_or_else(|| io::Error::other("direct process lifetime is unavailable"))
            .and_then(|lifetime| platform::finish_after_root_exit(self.pid, lifetime));
        self.guardian_stdin.take();
        await_drains(std::mem::take(&mut self.drains)).await;
        self.child.take();
        self.settled = true;
        cleanup.map_err(|error| io::Error::new(error.kind(), "direct process tree cleanup failed"))
    }
}

impl fmt::Debug for SupervisedDirectProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SupervisedDirectProcess")
            .field("pid", &self.pid)
            .field("settled", &self.settled)
            .finish_non_exhaustive()
    }
}

impl Drop for SupervisedDirectProcess {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.guardian_stdin.take();
        #[cfg(target_os = "windows")]
        if let Some(lifetime) = self.lifetime.as_ref() {
            let _ = platform::terminate_lifetime_now(lifetime);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = platform::terminate_tree_now(self.pid, &self.identity);
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
        self.lifetime.take();
        for drain in self.drains.drain(..) {
            drain.abort();
        }
        self.settled = true;
    }
}

async fn stop_uncontained_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn spawn_drain<R>(
    mut reader: R,
    capture: ProcessOutputCapture,
    stream: ProcessStream,
) -> JoinHandle<io::Result<()>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                return Ok(());
            }
            capture.append(stream, &buffer[..read]);
        }
    })
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    const RPC_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn request_debug_redacts_arguments_environment_and_rpc_token() {
        let executable = std::env::current_exe().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let bootstrap = CodeRpcBootstrap::new(8642, RPC_TOKEN).unwrap();
        let request = DirectProcessRequest::new(
            &executable,
            &cwd,
            [OsString::from("print('private code')")],
            [(
                OsString::from("SAFE_VALUE"),
                OsString::from("private-value"),
            )],
            Some(bootstrap),
        )
        .unwrap();

        let debug = format!("{request:?}");
        assert!(!debug.contains("private code"));
        assert!(!debug.contains("private-value"));
        assert!(!debug.contains(RPC_TOKEN));
        assert!(debug.contains("[redacted"));
    }

    #[tokio::test]
    async fn supervised_direct_process_keeps_stdout_and_stderr_separate() {
        if guardian_backend_executable().is_err() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let (executable, arguments) = output_command(workspace.path());
        let request = DirectProcessRequest::new(
            &executable,
            workspace.path(),
            arguments,
            sanitized_environment(),
            None,
        )
        .unwrap();
        let mut process = SupervisedDirectProcess::spawn(request).await.unwrap();
        assert_ne!(process.pid(), 0);
        let status = tokio::time::timeout(Duration::from_secs(10), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        let output = process
            .captured_output(&str::to_owned, 1024, DIRECT_PROCESS_STDERR_LIMIT_BYTES)
            .unwrap();
        assert_eq!(output.stdout.text, "direct-stdout");
        assert_eq!(output.stderr.text, "direct-stderr");
    }

    #[tokio::test]
    async fn dropping_supervisor_stops_direct_process_tree() {
        if guardian_backend_executable().is_err() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let heartbeat = workspace.path().join("heartbeat.txt");
        let (executable, arguments) = heartbeat_command(workspace.path(), &heartbeat);
        let request = DirectProcessRequest::new(
            &executable,
            workspace.path(),
            arguments,
            sanitized_environment(),
            None,
        )
        .unwrap();
        let process = SupervisedDirectProcess::spawn(request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if std::fs::metadata(&heartbeat).is_ok_and(|metadata| metadata.len() >= 2) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();

        let drop_started_at = Instant::now();
        drop(process);
        assert!(
            drop_started_at.elapsed() < Duration::from_secs(2),
            "dropping a direct process must not wait on external tree termination"
        );
        let stopped_at = std::fs::metadata(&heartbeat).unwrap().len();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), stopped_at);
    }

    #[cfg(target_os = "windows")]
    fn output_command(workspace: &Path) -> (std::path::PathBuf, Vec<OsString>) {
        let script = workspace.join("direct-output.cmd");
        std::fs::write(
            &script,
            "@echo off\r\n<nul set /p \"=direct-stdout\"\r\n>&2 <nul set /p \"=direct-stderr\"\r\nexit /b 0\r\n",
        )
        .unwrap();
        let executable = std::path::PathBuf::from(std::env::var_os("COMSPEC").unwrap());
        (
            executable,
            vec![
                OsString::from("/D"),
                OsString::from("/Q"),
                OsString::from("/C"),
                script.into_os_string(),
            ],
        )
    }

    #[cfg(unix)]
    fn output_command(_workspace: &Path) -> (std::path::PathBuf, Vec<OsString>) {
        (
            std::path::PathBuf::from("/bin/sh"),
            ["-c", "printf direct-stdout; printf direct-stderr >&2"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
    }

    #[cfg(target_os = "windows")]
    fn heartbeat_command(
        workspace: &Path,
        heartbeat: &Path,
    ) -> (std::path::PathBuf, Vec<OsString>) {
        let script = workspace.join("heartbeat.cmd");
        std::fs::write(
            &script,
            format!(
                "@echo off\r\n:loop\r\n<nul set /p =x>>\"{}\"\r\nping -n 2 127.0.0.1 >nul\r\ngoto loop\r\n",
                heartbeat.display()
            ),
        )
        .unwrap();
        (
            std::path::PathBuf::from(std::env::var_os("COMSPEC").unwrap()),
            vec![
                OsString::from("/D"),
                OsString::from("/Q"),
                OsString::from("/C"),
                script.into_os_string(),
            ],
        )
    }

    #[cfg(unix)]
    fn heartbeat_command(
        workspace: &Path,
        heartbeat: &Path,
    ) -> (std::path::PathBuf, Vec<OsString>) {
        let script = workspace.join("heartbeat.sh");
        let quoted = heartbeat.to_string_lossy().replace('\'', "'\\''");
        std::fs::write(
            &script,
            format!("while :; do printf x >> '{quoted}'; sleep 0.1; done\n"),
        )
        .unwrap();
        (
            std::path::PathBuf::from("/bin/sh"),
            vec![script.into_os_string()],
        )
    }
}
