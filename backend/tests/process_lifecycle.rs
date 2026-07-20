use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const RESTARTED_TOKEN: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn managed_process_exits_cleanly_when_the_parent_pipe_closes() {
    let address = unused_loopback_address();
    let hermes_home = tempfile::tempdir().expect("an isolated HERMES_HOME should be available");
    let (guard, stdin) = spawn_backend(address, hermes_home.path(), TOKEN);

    let capabilities = request(address, "/api/v1/capabilities", Some(TOKEN))
        .expect("authenticated capabilities should respond");
    assert!(capabilities.starts_with(b"HTTP/1.1 200"));

    shutdown_backend(guard, stdin);
}

#[test]
fn session_http_state_survives_process_restart_and_old_cursors_do_not() {
    let hermes_home = tempfile::tempdir().expect("an isolated HERMES_HOME should be available");
    let first_address = unused_loopback_address();
    let (first_guard, first_stdin) = spawn_backend(first_address, hermes_home.path(), TOKEN);

    let first_body = r#"{"profileId":"default","title":"Persistent HTTP session"}"#;
    let first = request_with(
        first_address,
        "POST",
        "/api/v1/sessions",
        Some(TOKEN),
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "process-restart-first"),
        ],
        first_body.as_bytes(),
    )
    .expect("the first session should be created");
    assert!(first.starts_with(b"HTTP/1.1 201"));
    let first_json = response_json(&first);
    let session_id = first_json["id"]
        .as_str()
        .expect("the created session should have an ID")
        .to_owned();

    let second = request_with(
        first_address,
        "POST",
        "/api/v1/sessions",
        Some(TOKEN),
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "process-restart-second"),
        ],
        br#"{"profileId":"default","title":"Cursor boundary"}"#,
    )
    .expect("the second session should be created");
    assert!(second.starts_with(b"HTTP/1.1 201"));

    let first_page = request(
        first_address,
        "/api/v1/sessions?profileId=default&limit=1",
        Some(TOKEN),
    )
    .expect("the first session page should respond");
    assert!(first_page.starts_with(b"HTTP/1.1 200"));
    let cursor = response_json(&first_page)["nextCursor"]
        .as_str()
        .expect("two sessions with limit one must produce a cursor")
        .to_owned();

    shutdown_backend(first_guard, first_stdin);

    let restarted_address = unused_loopback_address();
    let (restarted_guard, restarted_stdin) =
        spawn_backend(restarted_address, hermes_home.path(), RESTARTED_TOKEN);
    let persisted = request(
        restarted_address,
        &format!("/api/v1/sessions/{session_id}"),
        Some(RESTARTED_TOKEN),
    )
    .expect("the persisted session should respond after restart");
    assert!(persisted.starts_with(b"HTTP/1.1 200"));
    assert_eq!(
        response_json(&persisted)["title"],
        "Persistent HTTP session"
    );

    let replay = request_with(
        restarted_address,
        "POST",
        "/api/v1/sessions",
        Some(RESTARTED_TOKEN),
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "process-restart-first"),
        ],
        first_body.as_bytes(),
    )
    .expect("the persisted idempotency record should replay after restart");
    assert!(replay.starts_with(b"HTTP/1.1 201"));
    assert_eq!(response_json(&replay)["id"], session_id);

    let stale_cursor = request(
        restarted_address,
        &format!("/api/v1/sessions?profileId=default&limit=1&cursor={cursor}"),
        Some(RESTARTED_TOKEN),
    )
    .expect("the stale cursor request should respond");
    assert!(stale_cursor.starts_with(b"HTTP/1.1 400"));
    assert_eq!(response_json(&stale_cursor)["code"], "invalid_cursor");

    shutdown_backend(restarted_guard, restarted_stdin);
}

fn unused_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port should be available");
    listener
        .local_addr()
        .expect("listener should have an address")
}

fn request(address: SocketAddr, path: &str, token: Option<&str>) -> Option<Vec<u8>> {
    request_with(address, "GET", path, token, &[], &[])
}

fn request_with(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(200)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
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
    stream.write_all(head.as_bytes()).ok()?;
    stream.write_all(body).ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    Some(response)
}

fn response_json(response: &[u8]) -> serde_json::Value {
    let body_offset = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| offset + 4)
        .expect("an HTTP response must contain a header terminator");
    serde_json::from_slice(&response[body_offset..]).expect("the response body should be JSON")
}

fn spawn_backend(address: SocketAddr, hermes_home: &Path, token: &str) -> (ChildGuard, ChildStdin) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_synthchat-hermes-backend"))
        .env_remove("SYNTHCHAT_DESKTOP_TOKEN")
        .env("SYNTHCHAT_BACKEND_ADDR", address.to_string())
        .env("HERMES_HOME", hermes_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("backend process should start");
    let mut stdin = child.stdin.take().expect("backend stdin should be piped");
    stdin
        .write_all(format!("{token}\n").as_bytes())
        .expect("desktop token should be written");
    let guard = ChildGuard(Some(child));

    wait_until(Duration::from_secs(5), || {
        request(address, "/health", None)
            .is_some_and(|response| response.starts_with(b"HTTP/1.1 200"))
    });
    (guard, stdin)
}

fn shutdown_backend(mut guard: ChildGuard, stdin: ChildStdin) {
    drop(stdin);
    let status = wait_for_exit(
        guard.0.as_mut().expect("guard should own the child"),
        Duration::from_secs(5),
    );
    assert!(
        status.success(),
        "backend should exit cleanly after stdin EOF"
    );
    guard.0.take();
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("condition was not met before timeout");
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
