//! Process daemonization and IPC for background operation.

use crate::config::Config;

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use std::path::PathBuf;
use std::time::Instant;

/// Commands sent from CLI client to the running daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum IpcCommand {
    Shutdown,
    Status,
}

/// Responses from the daemon back to the CLI client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok,
    Status { pid: u32, uptime_seconds: u64 },
    Error { message: String },
}

/// Paths for daemon runtime files, all derived from the instance directory.
pub struct DaemonPaths {
    pub pid_file: PathBuf,
    pub socket: PathBuf,
    pub log_dir: PathBuf,
}

impl DaemonPaths {
    pub fn new(instance_dir: &std::path::Path) -> Self {
        Self {
            pid_file: instance_dir.join("spacebot.pid"),
            socket: instance_dir.join("spacebot.sock"),
            log_dir: instance_dir.join("logs"),
        }
    }

    pub fn from_default() -> Self {
        Self::new(&Config::default_instance_dir())
    }
}

/// Check whether a daemon is already running by testing PID file liveness
/// and socket connectivity.
pub fn is_running(paths: &DaemonPaths) -> Option<u32> {
    let pid = read_pid_file(&paths.pid_file)?;

    // Verify the process is actually alive
    if !is_process_alive(pid) {
        cleanup_stale_files(paths);
        return None;
    }

    // Double-check by trying to connect to the socket
    if paths.socket.exists() {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(&paths.socket) {
            drop(stream);
            return Some(pid);
        }
        // Socket exists but can't connect — stale
        cleanup_stale_files(paths);
        return None;
    }

    // PID alive but no socket — process may be starting up or crashed
    // without cleanup. Trust the PID.
    Some(pid)
}

/// Daemonize the current process. Returns in the child; the parent prints
/// a message and exits.
pub fn daemonize(paths: &DaemonPaths) -> anyhow::Result<()> {
    std::fs::create_dir_all(&paths.log_dir).with_context(|| {
        format!(
            "failed to create log directory: {}",
            paths.log_dir.display()
        )
    })?;

    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_dir.join("spacebot.out"))
        .context("failed to open stdout log")?;

    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_dir.join("spacebot.err"))
        .context("failed to open stderr log")?;

    let daemonize = daemonize::Daemonize::new()
        .pid_file(&paths.pid_file)
        .chown_pid_file(true)
        .stdout(stdout)
        .stderr(stderr);

    daemonize
        .start()
        .map_err(|error| anyhow!("failed to daemonize: {error}"))?;

    Ok(())
}

/// Initialize tracing for background mode with daily log rotation.
pub fn init_background_tracing(paths: &DaemonPaths, debug: bool) {
    let file_appender = tracing_appender::rolling::daily(&paths.log_dir, "spacebot.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Leak the guard so the non-blocking writer lives for the entire process.
    // The process owns this — it's cleaned up on exit.
    std::mem::forget(_guard);

    let filter = if debug {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::new("info")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();
}

/// Initialize tracing for foreground mode (stdout).
pub fn init_foreground_tracing(debug: bool) {
    let filter = if debug {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::new("info")
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Start the IPC server. Returns a shutdown receiver that the main event
/// loop should select on.
pub async fn start_ipc_server(
    paths: &DaemonPaths,
) -> anyhow::Result<(watch::Receiver<bool>, tokio::task::JoinHandle<()>)> {
    // Clean up any stale socket file
    if paths.socket.exists() {
        std::fs::remove_file(&paths.socket).with_context(|| {
            format!("failed to remove stale socket: {}", paths.socket.display())
        })?;
    }

    let listener = UnixListener::bind(&paths.socket)
        .with_context(|| format!("failed to bind IPC socket: {}", paths.socket.display()))?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let start_time = Instant::now();
    let socket_path = paths.socket.clone();

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _address)) => {
                    let shutdown_tx = shutdown_tx.clone();
                    let uptime = start_time.elapsed();
                    tokio::spawn(async move {
                        if let Err(error) =
                            handle_ipc_connection(stream, &shutdown_tx, uptime).await
                        {
                            tracing::warn!(%error, "IPC connection handler failed");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to accept IPC connection");
                }
            }
        }
    });

    // Spawn a cleanup task that removes the socket file when the server shuts down
    let cleanup_socket = socket_path.clone();
    let mut cleanup_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        let _ = cleanup_rx.wait_for(|shutdown| *shutdown).await;
        let _ = std::fs::remove_file(&cleanup_socket);
    });

    Ok((shutdown_rx, handle))
}

/// Handle a single IPC client connection.
async fn handle_ipc_connection(
    stream: UnixStream,
    shutdown_tx: &watch::Sender<bool>,
    uptime: std::time::Duration,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let command: IpcCommand = serde_json::from_str(line.trim())
        .with_context(|| format!("invalid IPC command: {line}"))?;

    let response = match command {
        IpcCommand::Shutdown => {
            tracing::info!("shutdown requested via IPC");
            shutdown_tx.send(true).ok();
            IpcResponse::Ok
        }
        IpcCommand::Status => IpcResponse::Status {
            pid: std::process::id(),
            uptime_seconds: uptime.as_secs(),
        },
    };

    let mut response_bytes = serde_json::to_vec(&response)?;
    response_bytes.push(b'\n');
    writer.write_all(&response_bytes).await?;
    writer.flush().await?;

    Ok(())
}

/// Send a command to the running daemon and return the response.
pub async fn send_command(paths: &DaemonPaths, command: IpcCommand) -> anyhow::Result<IpcResponse> {
    let stream = UnixStream::connect(&paths.socket)
        .await
        .with_context(|| "failed to connect to spacebot daemon. is it running?")?;

    let (reader, mut writer) = stream.into_split();

    let mut command_bytes = serde_json::to_vec(&command)?;
    command_bytes.push(b'\n');
    writer.write_all(&command_bytes).await?;
    writer.flush().await?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: IpcResponse = serde_json::from_str(line.trim())
        .with_context(|| format!("invalid IPC response: {line}"))?;

    Ok(response)
}

/// Clean up PID and socket files on shutdown.
pub fn cleanup(paths: &DaemonPaths) {
    if let Err(error) = std::fs::remove_file(&paths.pid_file) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, "failed to remove PID file");
        }
    }
    if let Err(error) = std::fs::remove_file(&paths.socket) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, "failed to remove socket file");
        }
    }
}

fn read_pid_file(path: &std::path::Path) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u32>().ok()
}

fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if the process exists without sending a signal
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn cleanup_stale_files(paths: &DaemonPaths) {
    let _ = std::fs::remove_file(&paths.pid_file);
    let _ = std::fs::remove_file(&paths.socket);
}

/// Wait for the daemon process to exit after sending a shutdown command.
/// Polls the PID with a short interval, times out after 10 seconds.
pub fn wait_for_exit(pid: u32) -> bool {
    for _ in 0..100 {
        if !is_process_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}
