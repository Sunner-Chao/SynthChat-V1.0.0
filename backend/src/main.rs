use std::{
    error::Error,
    io::{Read, Write},
    net::SocketAddr,
    thread,
};

use synthchat_hermes_backend::{
    RuntimeConfig, build_router_with_shutdown, process_guardian_mode_requested,
    run_process_guardian_stdio,
};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

const PRESERVE_RUNS_SHUTDOWN_COMMAND: &[u8] = b"SYNTHCHAT_PRESERVE_RUNS\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShutdownMode {
    Drain,
    PreserveRuns,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if process_guardian_mode_requested() {
        std::process::exit(run_process_guardian_stdio());
    }

    init_tracing();

    let runtime = RuntimeConfig::from_env_or_stdin()?;
    let bind_addr = runtime.bind_addr();
    let watch_parent_stdin = runtime.watch_parent_stdin();
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;
    let (app, app_shutdown) = build_router_with_shutdown(runtime.into_app_config());

    emit_startup_handshake(local_addr)?;
    info!(address = %local_addr, "backend listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            match shutdown_signal(watch_parent_stdin).await {
                ShutdownMode::Drain => app_shutdown.shutdown().await,
                ShutdownMode::PreserveRuns => app_shutdown.shutdown_preserving_runs().await,
            }
        })
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn emit_startup_handshake(address: SocketAddr) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(startup_handshake(address).as_bytes())?;
    stdout.flush()
}

fn startup_handshake(address: SocketAddr) -> String {
    format!("SYNTHCHAT_BACKEND_READY {address}\n")
}

async fn shutdown_signal(watch_parent_stdin: bool) -> ShutdownMode {
    if watch_parent_stdin {
        tokio::select! {
            () = ctrl_c_signal() => ShutdownMode::Drain,
            mode = parent_stdin_closed() => mode,
        }
    } else {
        ctrl_c_signal().await;
        ShutdownMode::Drain
    }
}

async fn ctrl_c_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(%error, "failed to install Ctrl+C handler");
    }
}

async fn parent_stdin_closed() -> ShutdownMode {
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let monitor = thread::Builder::new()
        .name("synthchat-parent-pipe".to_owned())
        .spawn(move || {
            let mode = read_parent_shutdown_mode(std::io::stdin());
            let _ = closed_tx.send(mode);
        });

    if let Err(error) = monitor {
        tracing::warn!(%error, "failed to start parent stdin monitor");
        return ShutdownMode::Drain;
    }

    match closed_rx.await {
        Ok(mode) => mode,
        Err(_) => {
            tracing::warn!("parent stdin monitor stopped unexpectedly");
            ShutdownMode::Drain
        }
    }
}

fn read_parent_shutdown_mode(mut input: impl Read) -> ShutdownMode {
    let mut command = Vec::with_capacity(PRESERVE_RUNS_SHUTDOWN_COMMAND.len());
    if input
        .by_ref()
        .take((PRESERVE_RUNS_SHUTDOWN_COMMAND.len() + 1) as u64)
        .read_to_end(&mut command)
        .is_ok()
        && command == PRESERVE_RUNS_SHUTDOWN_COMMAND
    {
        ShutdownMode::PreserveRuns
    } else {
        ShutdownMode::Drain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_handshake_contains_the_actual_bound_address() {
        let address = "127.0.0.1:49152".parse().unwrap();
        assert_eq!(
            startup_handshake(address),
            "SYNTHCHAT_BACKEND_READY 127.0.0.1:49152\n"
        );
    }

    #[test]
    fn parent_shutdown_mode_is_explicit_and_bounded() {
        assert_eq!(read_parent_shutdown_mode(&b""[..]), ShutdownMode::Drain);
        assert_eq!(
            read_parent_shutdown_mode(PRESERVE_RUNS_SHUTDOWN_COMMAND),
            ShutdownMode::PreserveRuns
        );
        assert_eq!(
            read_parent_shutdown_mode(b"SYNTHCHAT_PRESERVE_RUNS\nextra".as_slice()),
            ShutdownMode::Drain
        );
    }
}
