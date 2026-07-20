use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::json;
use uuid::Uuid;

const READY_PREFIX: &str = "SYNTHCHAT_BACKEND_READY ";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn trace_runtime_logs_do_not_expose_request_credentials_or_profile_secrets() {
    let hermes_home = tempfile::tempdir().expect("an isolated HERMES_HOME should be available");
    let desktop_token = generated_marker("desktop-token");
    let rejected_bearer = generated_marker("rejected-bearer");
    let credential_header = generated_marker("credential-header");
    let profile_secret = generated_marker("sk-profile-secret");
    let secret_name = format!("LOG_REDACTION_{}", Uuid::new_v4().simple()).to_uppercase();
    let secret_path = format!("/api/v1/profiles/default/secrets/{secret_name}");

    let mut backend = BackendProcess::spawn(hermes_home.path(), &desktop_token);
    let address = backend.address;
    let mut secret_cleanup =
        SecretCleanupGuard::new(address, desktop_token.clone(), secret_path.clone());

    let health = wait_for_health(address);
    assert_status(&health, 200, "health check");

    let authenticated = request(
        address,
        "GET",
        "/api/v1/capabilities",
        Some(&desktop_token),
        &[("X-Api-Key", credential_header.as_str())],
        &[],
    )
    .expect("authenticated capabilities should respond");
    assert_status(&authenticated, 200, "authenticated capabilities");

    let rejected = request(
        address,
        "GET",
        "/api/v1/capabilities",
        Some(&rejected_bearer),
        &[],
        &[],
    )
    .expect("rejected authentication should respond");
    assert_status(&rejected, 401, "rejected authentication");

    let secret_body = json!({"value": &profile_secret}).to_string();
    let secret_write = request(
        address,
        "PUT",
        &secret_path,
        Some(&desktop_token),
        &[("Content-Type", "application/json")],
        secret_body.as_bytes(),
    )
    .expect("profile secret write should respond");

    match secret_write.status {
        200 => {
            secret_cleanup.arm();
            let delete = request(
                address,
                "DELETE",
                &secret_path,
                Some(&desktop_token),
                &[],
                &[],
            )
            .expect("profile secret cleanup should respond");
            assert_status(&delete, 204, "profile secret cleanup");
            secret_cleanup.disarm();
        }
        503 => {
            let body: serde_json::Value = serde_json::from_slice(secret_write.body())
                .expect("the unavailable keychain response should be JSON");
            assert_eq!(body["code"], "secret_storage_unavailable");
        }
        status => panic!("profile secret write returned unexpected HTTP status {status}"),
    }

    let logs = backend.shutdown();
    assert!(
        String::from_utf8_lossy(&logs.stdout).contains(READY_PREFIX),
        "stdout should contain the bounded readiness handshake"
    );
    let stderr = String::from_utf8_lossy(&logs.stderr);
    assert!(
        stderr.contains("backend listening"),
        "stderr should contain initialized runtime tracing"
    );
    assert!(
        stderr.contains("/api/v1/capabilities"),
        "trace logging should observe the protected request"
    );

    for (label, marker) in [
        ("desktop token", desktop_token.as_str()),
        ("rejected Bearer value", rejected_bearer.as_str()),
        ("credential header", credential_header.as_str()),
        ("Profile secret", profile_secret.as_str()),
    ] {
        assert_log_omits(&logs.stdout, "stdout", label, marker);
        assert_log_omits(&logs.stderr, "stderr", label, marker);
    }
}

fn generated_marker(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

struct BackendProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout_reader: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_reader: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    address: SocketAddr,
}

impl BackendProcess {
    fn spawn(hermes_home: &Path, desktop_token: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_synthchat-hermes-backend"))
            .env_remove("SYNTHCHAT_DESKTOP_TOKEN")
            .env_remove("SYNTHCHAT_ALLOWED_ORIGINS")
            .env_remove("SYNTHCHAT_SKILL_GITHUB_API_BASE_URL")
            .env_remove("SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL")
            .env_remove("SYNTHCHAT_SKILL_REGISTRY_INDEX_URL")
            .env_remove("SYNTHCHAT_TAVILY_BASE_URL")
            .env("SYNTHCHAT_BACKEND_ADDR", "127.0.0.1:0")
            .env("HERMES_HOME", hermes_home)
            // Code execution discovery is unrelated to log redaction and can
            // make this process-level test depend on a cold host PATH scan.
            .env("PATH", "")
            .env("RUST_LOG", "trace")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("backend process should start");

        let stdin = child.stdin.take().expect("backend stdin should be piped");
        let stdout = child.stdout.take().expect("backend stdout should be piped");
        let stderr = child.stderr.take().expect("backend stderr should be piped");
        let (ready, stdout_reader) = capture_stdout(stdout);
        let stderr_reader = capture_stderr(stderr);
        let mut process = Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            address: "127.0.0.1:0"
                .parse()
                .expect("fallback address should parse"),
        };

        process
            .stdin
            .as_mut()
            .expect("backend stdin should remain open")
            .write_all(format!("{desktop_token}\n").as_bytes())
            .expect("desktop token should be written");
        process
            .stdin
            .as_mut()
            .expect("backend stdin should remain open")
            .flush()
            .expect("desktop token should be flushed");

        let handshake = ready
            .recv_timeout(PROCESS_TIMEOUT)
            .expect("backend should emit its readiness handshake");
        process.address = parse_ready_address(&handshake);
        process
    }

    fn shutdown(&mut self) -> CapturedLogs {
        drop(self.stdin.take());
        let status = wait_for_exit(
            self.child
                .as_mut()
                .expect("backend child should be present"),
            PROCESS_TIMEOUT,
        );
        assert!(
            status.success(),
            "backend should exit cleanly after stdin EOF"
        );
        self.child.take();

        CapturedLogs {
            stdout: join_capture(self.stdout_reader.take(), "stdout"),
            stderr: join_capture(self.stderr_reader.take(), "stderr"),
        }
    }
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child.take();
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

struct SecretCleanupGuard {
    address: SocketAddr,
    bearer: String,
    path: String,
    armed: bool,
}

impl SecretCleanupGuard {
    fn new(address: SocketAddr, bearer: String, path: String) -> Self {
        Self {
            address,
            bearer,
            path,
            armed: false,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SecretCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = request(
                self.address,
                "DELETE",
                &self.path,
                Some(&self.bearer),
                &[],
                &[],
            );
        }
    }
}

struct CapturedLogs {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct HttpResponse {
    status: u16,
    bytes: Vec<u8>,
}

impl HttpResponse {
    fn body(&self) -> &[u8] {
        let offset = self
            .bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|offset| offset + 4)
            .expect("an HTTP response should contain a header terminator");
        &self.bytes[offset..]
    }
}

fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<HttpResponse> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(PROCESS_TIMEOUT))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    if let Some(bearer) = bearer {
        head.push_str("Authorization: Bearer ");
        head.push_str(bearer);
        head.push_str("\r\n");
    }
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let status = parse_status(&bytes)?;
    Ok(HttpResponse { status, bytes })
}

fn wait_for_health(address: SocketAddr) -> HttpResponse {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Ok(response) = request(address, "GET", "/health", None, &[], &[])
            && response.status == 200
        {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "backend health check did not become ready"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn parse_status(response: &[u8]) -> std::io::Result<u16> {
    let first_line = response
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or_else(|| std::io::Error::other("missing HTTP status line"))?;
    let first_line = std::str::from_utf8(first_line)
        .map_err(|_| std::io::Error::other("HTTP status line was not UTF-8"))?;
    first_line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or_else(|| std::io::Error::other("missing HTTP status code"))?
        .parse()
        .map_err(|_| std::io::Error::other("invalid HTTP status code"))
}

fn assert_status(response: &HttpResponse, expected: u16, operation: &str) {
    assert_eq!(
        response.status, expected,
        "{operation} returned an unexpected HTTP status"
    );
}

fn parse_ready_address(handshake: &str) -> SocketAddr {
    let address = handshake
        .strip_prefix(READY_PREFIX)
        .and_then(|value| value.trim().parse::<SocketAddr>().ok())
        .expect("backend readiness handshake should contain a socket address");
    assert!(address.ip().is_loopback());
    assert_ne!(address.port(), 0);
    address
}

fn capture_stdout(stdout: ChildStdout) -> (Receiver<String>, JoinHandle<std::io::Result<Vec<u8>>>) {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut handshake = String::new();
        stdout.read_line(&mut handshake)?;
        let _ = ready_tx.send(handshake.clone());
        let mut captured = handshake.into_bytes();
        stdout.read_to_end(&mut captured)?;
        Ok(captured)
    });
    (ready_rx, reader)
}

fn capture_stderr(mut stderr: ChildStderr) -> JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        stderr.read_to_end(&mut captured)?;
        Ok(captured)
    })
}

fn join_capture(reader: Option<JoinHandle<std::io::Result<Vec<u8>>>>, stream: &str) -> Vec<u8> {
    reader
        .unwrap_or_else(|| panic!("{stream} capture should be present"))
        .join()
        .unwrap_or_else(|_| panic!("{stream} capture thread should not panic"))
        .unwrap_or_else(|_| panic!("{stream} capture should complete"))
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "backend did not exit before timeout"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn assert_log_omits(log: &[u8], stream: &str, label: &str, marker: &str) {
    assert!(
        !log.windows(marker.len())
            .any(|window| window == marker.as_bytes()),
        "{stream} contained the raw {label}"
    );
}
