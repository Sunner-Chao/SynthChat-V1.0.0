use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};

use synthchat_hermes_backend::{
    PROCESS_GUARDIAN_PROTOCOL_EXIT, ProcessGuardianLaunch, encode_process_guardian_launch,
    encode_process_guardian_stdin, encode_process_guardian_stdin_close, process_guardian_command,
};
use tokio::{io::AsyncWriteExt, time::timeout};

const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

// Each case owns a real guardian plus a shell process tree. Serialize the
// external fixtures so a parent-disconnect teardown cannot consume another
// case's protocol deadline.
static GUARDIAN_FIXTURE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn lock_guardian_fixture() -> tokio::sync::MutexGuard<'static, ()> {
    GUARDIAN_FIXTURE_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[tokio::test]
async fn guardian_eof_before_complete_launch_frame_has_no_side_effect() {
    let _fixture = lock_guardian_fixture().await;
    let workspace = tempfile::tempdir().expect("an isolated guardian cwd should be available");
    let marker = workspace.path().join("must-not-exist.txt");
    let script = format!("printf guardian-ran > {}", bash_quote(&marker));
    let launch = ProcessGuardianLaunch::new(
        &bash_executable(),
        workspace.path(),
        script,
        safe_environment(),
    )
    .expect("the guardian launch should be valid");
    let frame = encode_process_guardian_launch(&launch).expect("the launch frame should encode");

    let mut child = guardian_command()
        .spawn()
        .expect("the guardian helper should start");
    let mut stdin = child.stdin.take().expect("guardian stdin should be piped");
    stdin
        .write_all(&frame[..frame.len() - 1])
        .await
        .expect("the partial frame should be writable");
    drop(stdin);

    let output = timeout(HELPER_TIMEOUT, child.wait_with_output())
        .await
        .expect("the guardian should not hang on an incomplete frame")
        .expect("the guardian should return an exit status");
    assert_eq!(output.status.code(), Some(PROCESS_GUARDIAN_PROTOCOL_EXIT));
    assert!(
        !marker.exists(),
        "the shell must not start before a complete validated launch frame"
    );
}

#[tokio::test]
async fn guardian_launches_after_frame_and_forwards_remaining_stdin() {
    let _fixture = lock_guardian_fixture().await;
    let workspace = tempfile::tempdir().expect("an isolated guardian cwd should be available");
    let launch = ProcessGuardianLaunch::new(
        &bash_executable(),
        workspace.path(),
        "IFS= read -r line; printf 'guardian:%s' \"$line\"; exit 7",
        safe_environment(),
    )
    .expect("the guardian launch should be valid");
    let frame = encode_process_guardian_launch(&launch).expect("the launch frame should encode");

    let mut child = guardian_command()
        .spawn()
        .expect("the guardian helper should start");
    let mut stdin = child.stdin.take().expect("guardian stdin should be piped");
    stdin
        .write_all(&frame)
        .await
        .expect("the launch frame should be writable");
    stdin
        .write_all(
            &encode_process_guardian_stdin(b"payload after durable commit\n")
                .expect("stdin control should encode"),
        )
        .await
        .expect("process stdin should be writable after the frame");

    let output = timeout(HELPER_TIMEOUT, child.wait_with_output())
        .await
        .expect("the guardian child should complete")
        .expect("the guardian should return its child status");
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "guardian:payload after durable commit"
    );
    drop(stdin);
}

#[tokio::test]
async fn guardian_parent_disconnect_stops_the_running_command_tree() {
    let _fixture = lock_guardian_fixture().await;
    let workspace = tempfile::tempdir().expect("an isolated guardian cwd should be available");
    let heartbeat = workspace.path().join("heartbeat.txt");
    let script = format!(
        "while :; do printf x >> {}; sleep 0.1; done",
        bash_quote(&heartbeat)
    );
    let launch = ProcessGuardianLaunch::new(
        &bash_executable(),
        workspace.path(),
        script,
        safe_environment(),
    )
    .expect("the guardian launch should be valid");
    let frame = encode_process_guardian_launch(&launch).expect("the launch frame should encode");
    let mut child = guardian_command()
        .spawn()
        .expect("the guardian helper should start");
    let mut stdin = child.stdin.take().expect("guardian stdin should be piped");
    stdin
        .write_all(&frame)
        .await
        .expect("the launch frame should be writable");
    timeout(HELPER_TIMEOUT, async {
        loop {
            if std::fs::metadata(&heartbeat).is_ok_and(|metadata| metadata.len() >= 2) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the guarded command should start");

    drop(stdin);
    timeout(HELPER_TIMEOUT, child.wait())
        .await
        .expect("the guardian should stop after parent disconnect")
        .expect("the guardian exit should be observable");
    let stopped_at = std::fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        std::fs::metadata(&heartbeat).unwrap().len(),
        stopped_at,
        "the guarded command must not outlive its parent control pipe"
    );
}

#[tokio::test]
async fn guardian_close_control_delivers_child_stdin_eof_without_disconnect() {
    let _fixture = lock_guardian_fixture().await;
    let workspace = tempfile::tempdir().expect("an isolated guardian cwd should be available");
    let launch = ProcessGuardianLaunch::new(
        &bash_executable(),
        workspace.path(),
        "cat >/dev/null; printf stdin-closed",
        safe_environment(),
    )
    .expect("the guardian launch should be valid");
    let frame = encode_process_guardian_launch(&launch).expect("the launch frame should encode");
    let mut child = guardian_command()
        .spawn()
        .expect("the guardian helper should start");
    let mut stdin = child.stdin.take().expect("guardian stdin should be piped");
    stdin
        .write_all(&frame)
        .await
        .expect("the launch frame should be writable");
    stdin
        .write_all(&encode_process_guardian_stdin_close())
        .await
        .expect("the close control should be writable");
    let output = timeout(HELPER_TIMEOUT, child.wait_with_output())
        .await
        .expect("the child should observe stdin EOF without parent disconnect")
        .expect("the guardian should return its child status");
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "stdin-closed");
    drop(stdin);
}

fn guardian_command() -> tokio::process::Command {
    let backend = Path::new(env!("CARGO_BIN_EXE_synthchat-hermes-backend"));
    let mut command = process_guardian_command(backend, false)
        .expect("the backend path should produce a guardian command");
    command.stderr(Stdio::piped());
    command
}

fn safe_environment() -> Vec<(String, String)> {
    env::var("PATH")
        .ok()
        .map(|path| vec![("PATH".to_owned(), path)])
        .unwrap_or_default()
}

fn bash_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(target_os = "windows")]
fn bash_executable() -> PathBuf {
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root);
            if variable == "LOCALAPPDATA" {
                candidates.push(root.join("Programs/Git/bin/bash.exe"));
            } else {
                candidates.push(root.join("Git/bin/bash.exe"));
                candidates.push(root.join("Git/usr/bin/bash.exe"));
            }
        }
    }
    let output = std::process::Command::new("where.exe")
        .arg("git.exe")
        .output()
        .expect("where.exe should be available on Windows");
    if output.status.success() {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(root) = Path::new(line.trim()).parent().and_then(Path::parent) {
                candidates.push(root.join("bin/bash.exe"));
                candidates.push(root.join("usr/bin/bash.exe"));
            }
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .expect("Git Bash is required for the guardian integration tests")
}

#[cfg(not(target_os = "windows"))]
fn bash_executable() -> PathBuf {
    [PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin/bash")]
        .into_iter()
        .find(|candidate| candidate.is_file())
        .expect("bash is required for the guardian integration tests")
}
