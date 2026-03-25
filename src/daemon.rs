//! Process daemonization and IPC for background operation.

use crate::config::{Config, TelemetryConfig};

use anyhow::Context as _;
#[cfg(unix)]
use anyhow::anyhow;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_sdk::trace::SdkTracerProvider;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tracing_subscriber::fmt::format;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

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

fn truncate_for_log(message: &str, max_chars: usize) -> (&str, bool) {
    match message.char_indices().nth(max_chars) {
        Some((byte_index, _character)) => (&message[..byte_index], true),
        None => (message, false),
    }
}

// ---------------------------------------------------------------------------
// Platform-specific: process liveness
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if the process exists without sending a signal
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::synchapi::WaitForSingleObject;
    use winapi::shared::winerror::WAIT_TIMEOUT;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x00100000;

    unsafe {
        let handle =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return false;
        }
        // Check if the process has exited (wait 0ms)
        let result = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        result == WAIT_TIMEOUT
    }
}

// ---------------------------------------------------------------------------
// Platform-specific: is_running check
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub fn is_running(paths: &DaemonPaths) -> Option<u32> {
    let pid = read_pid_file(&paths.pid_file)?;

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
        cleanup_stale_files(paths);
        return None;
    }

    Some(pid)
}

#[cfg(windows)]
pub fn is_running(paths: &DaemonPaths) -> Option<u32> {
    let pid = read_pid_file(&paths.pid_file)?;

    if !is_process_alive(pid) {
        cleanup_stale_files(paths);
        return None;
    }

    // On Windows, try to connect via TCP to verify the IPC server is up
    let port_file = paths.socket.with_extension("port");
    if let Some(port) = read_ipc_port(&port_file) {
        if let std::result::Result::Ok(stream) =
            std::net::TcpStream::connect(("127.0.0.1", port))
        {
            drop(stream);
            return Some(pid);
        }
        cleanup_stale_files(paths);
        return None;
    }

    Some(pid)
}

// ---------------------------------------------------------------------------
// Platform-specific: daemonize
// ---------------------------------------------------------------------------

#[cfg(unix)]
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

#[cfg(windows)]
pub fn daemonize(_paths: &DaemonPaths) -> anyhow::Result<()> {
    anyhow::bail!(
        "background daemonization is not supported on Windows. \
         Use --foreground (-f) to run in the foreground."
    );
}

// ---------------------------------------------------------------------------
// Platform-specific: IPC server (Unix sockets vs TCP)
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub async fn start_ipc_server(
    paths: &DaemonPaths,
) -> anyhow::Result<(watch::Receiver<bool>, tokio::task::JoinHandle<()>)> {
    use tokio::net::UnixListener;

    if let Some(parent) = paths.socket.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create instance directory: {}", parent.display())
        })?;
    }

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
                        let (reader, writer) = stream.into_split();
                        if let Err(error) =
                            handle_ipc_connection(reader, writer, &shutdown_tx, uptime).await
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

    let cleanup_socket = socket_path.clone();
    let mut cleanup_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        let _ = cleanup_rx.wait_for(|shutdown| *shutdown).await;
        let _ = std::fs::remove_file(&cleanup_socket);
    });

    Ok((shutdown_rx, handle))
}

#[cfg(windows)]
pub async fn start_ipc_server(
    paths: &DaemonPaths,
) -> anyhow::Result<(watch::Receiver<bool>, tokio::task::JoinHandle<()>)> {
    use tokio::net::TcpListener;

    if let Some(parent) = paths.socket.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create instance directory: {}", parent.display())
        })?;
    }

    // Bind to an ephemeral port on localhost
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind IPC TCP listener")?;
    let port = listener
        .local_addr()
        .context("failed to get local address")?
        .port();

    // Write the port so clients can find us
    let port_file = paths.socket.with_extension("port");
    std::fs::write(&port_file, port.to_string())
        .with_context(|| format!("failed to write port file: {}", port_file.display()))?;

    // Also write a PID file on Windows since daemonize doesn't do it for us
    std::fs::write(&paths.pid_file, std::process::id().to_string())
        .with_context(|| format!("failed to write PID file: {}", paths.pid_file.display()))?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let start_time = Instant::now();
    let port_file_cleanup = port_file.clone();

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _address)) => {
                    let shutdown_tx = shutdown_tx.clone();
                    let uptime = start_time.elapsed();
                    tokio::spawn(async move {
                        let (reader, writer) = stream.into_split();
                        if let Err(error) =
                            handle_ipc_connection(reader, writer, &shutdown_tx, uptime).await
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

    let mut cleanup_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        let _ = cleanup_rx.wait_for(|shutdown| *shutdown).await;
        let _ = std::fs::remove_file(&port_file_cleanup);
    });

    Ok((shutdown_rx, handle))
}

// ---------------------------------------------------------------------------
// Platform-specific: IPC client (send_command)
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub async fn send_command(paths: &DaemonPaths, command: IpcCommand) -> anyhow::Result<IpcResponse> {
    let stream = tokio::net::UnixStream::connect(&paths.socket)
        .await
        .with_context(|| "failed to connect to spacebot daemon. is it running?")?;

    let (reader, writer) = stream.into_split();
    send_command_over(reader, writer, command).await
}

#[cfg(windows)]
pub async fn send_command(paths: &DaemonPaths, command: IpcCommand) -> anyhow::Result<IpcResponse> {
    let port_file = paths.socket.with_extension("port");
    let port = read_ipc_port(&port_file)
        .with_context(|| "failed to read IPC port file. is spacebot running?")?;

    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| "failed to connect to spacebot daemon. is it running?")?;

    let (reader, writer) = stream.into_split();
    send_command_over(reader, writer, command).await
}

// ---------------------------------------------------------------------------
// Shared IPC helpers (generic over AsyncRead/AsyncWrite)
// ---------------------------------------------------------------------------

async fn handle_ipc_connection<R, W>(
    reader: R,
    mut writer: W,
    shutdown_tx: &watch::Sender<bool>,
    uptime: std::time::Duration,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
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

async fn send_command_over<R, W>(
    reader: R,
    mut writer: W,
    command: IpcCommand,
) -> anyhow::Result<IpcResponse>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
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

// ---------------------------------------------------------------------------
// Tracing (platform-agnostic)
// ---------------------------------------------------------------------------

/// Initialize tracing for background (daemon) mode.
///
/// Returns an `SdkTracerProvider` if OTLP export is configured. The caller must
/// hold onto it for the process lifetime and call `.shutdown()` before exit so
/// the batch exporter flushes buffered spans.
pub fn init_background_tracing(
    paths: &DaemonPaths,
    debug: bool,
    telemetry: &TelemetryConfig,
) -> Option<SdkTracerProvider> {
    let file_appender = tracing_appender::rolling::daily(&paths.log_dir, "spacebot.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    let field_formatter = format::debug_fn(|writer, field, value| {
        let field_name = field.name();

        if field_name == "gen_ai.system_instructions"
            || field_name == "gen_ai.tool.call.arguments"
            || field_name == "gen_ai.tool.call.result"
        {
            Ok(())
        } else if field_name == "message" {
            let formatted = format!("{value:?}");
            const MAX_MESSAGE_CHARS: usize = 280;
            let (truncated, was_truncated) = truncate_for_log(&formatted, MAX_MESSAGE_CHARS);
            if was_truncated {
                write!(writer, "{}={}...", field_name, truncated)
            } else {
                write!(writer, "{}={formatted}", field_name)
            }
        } else {
            write!(writer, "{}={value:?}", field_name)
        }
    });

    // Leak the guard so the non-blocking writer lives for the entire process.
    // The process owns this — it's cleaned up on exit.
    std::mem::forget(_guard);

    let filter = build_env_filter(debug);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .fmt_fields(field_formatter)
        .compact();

    match build_otlp_provider(telemetry) {
        Some(provider) => {
            let tracer = provider.tracer("spacebot");
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .init();
            Some(provider)
        }
        None => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .init();
            None
        }
    }
}

/// Initialize tracing for foreground (terminal) mode.
///
/// Returns an `SdkTracerProvider` if OTLP export is configured.
pub fn init_foreground_tracing(
    debug: bool,
    telemetry: &TelemetryConfig,
) -> Option<SdkTracerProvider> {
    let field_formatter = format::debug_fn(|writer, field, value| {
        let field_name = field.name();

        if field_name == "gen_ai.system_instructions"
            || field_name == "gen_ai.tool.call.arguments"
            || field_name == "gen_ai.tool.call.result"
        {
            Ok(())
        } else if field_name == "message" {
            let formatted = format!("{value:?}");
            const MAX_MESSAGE_CHARS: usize = 280;
            let (truncated, was_truncated) = truncate_for_log(&formatted, MAX_MESSAGE_CHARS);
            if was_truncated {
                write!(writer, "{}={}", field_name, truncated)?;
                write!(writer, "...")?;
            } else {
                write!(writer, "{}={formatted}", field_name)?;
            }
            Ok(())
        } else {
            write!(writer, "{}={value:?}", field_name)
        }
    });
    let filter = build_env_filter(debug);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .fmt_fields(field_formatter)
        .compact();

    match build_otlp_provider(telemetry) {
        Some(provider) => {
            let tracer = provider.tracer("spacebot");
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .init();
            Some(provider)
        }
        None => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .init();
            None
        }
    }
}

fn build_env_filter(debug: bool) -> tracing_subscriber::EnvFilter {
    if debug {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::new("info")
    }
}

/// Build an OTLP `SdkTracerProvider` when an endpoint is configured.
///
/// Returns `None` if neither the config field nor the `OTEL_EXPORTER_OTLP_ENDPOINT`
/// environment variable is set, allowing the OTel layer to be omitted entirely.
fn build_otlp_provider(telemetry: &TelemetryConfig) -> Option<SdkTracerProvider> {
    use opentelemetry_otlp::WithExportConfig as _;

    let endpoint = telemetry.otlp_endpoint.as_deref()?;

    // The HTTP/protobuf endpoint path is /v1/traces by default. Append it only
    // when the caller provided a bare host:port so both forms work.
    let endpoint = if endpoint.ends_with("/v1/traces") {
        endpoint.to_owned()
    } else {
        format!("{}/v1/traces", endpoint.trim_end_matches('/'))
    };

    let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint);
    if !telemetry.otlp_headers.is_empty() {
        exporter_builder = exporter_builder.with_headers(telemetry.otlp_headers.clone());
    }
    let exporter = exporter_builder
        .build()
        .map_err(|error| eprintln!("failed to build OTLP exporter: {error}"))
        .ok()?;

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(telemetry.service_name.clone())
        .build();

    let sampler: opentelemetry_sdk::trace::Sampler =
        if (telemetry.sample_rate - 1.0).abs() < f64::EPSILON {
            opentelemetry_sdk::trace::Sampler::AlwaysOn
        } else {
            opentelemetry_sdk::trace::Sampler::ParentBased(Box::new(
                opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(telemetry.sample_rate),
            ))
        };

    let batch_processor =
        opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
            exporter,
            opentelemetry_sdk::runtime::Tokio,
        )
        .build();

    let provider = SdkTracerProvider::builder()
        .with_span_processor(batch_processor)
        .with_resource(resource)
        .with_sampler(sampler)
        .build();

    Some(provider)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Clean up PID and socket files on shutdown.
pub fn cleanup(paths: &DaemonPaths) {
    if let Err(error) = std::fs::remove_file(&paths.pid_file)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "failed to remove PID file");
    }
    if let Err(error) = std::fs::remove_file(&paths.socket)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "failed to remove socket file");
    }
    // Also clean up the port file on Windows
    let port_file = paths.socket.with_extension("port");
    if let Err(error) = std::fs::remove_file(&port_file)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "failed to remove port file");
    }
}

fn read_pid_file(path: &std::path::Path) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u32>().ok()
}

fn cleanup_stale_files(paths: &DaemonPaths) {
    let _ = std::fs::remove_file(&paths.pid_file);
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(paths.socket.with_extension("port"));
}

#[cfg(windows)]
fn read_ipc_port(port_file: &std::path::Path) -> Option<u16> {
    let content = std::fs::read_to_string(port_file).ok()?;
    content.trim().parse::<u16>().ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_for_log_handles_multibyte_characters() {
        let message = "abc→def";
        let (truncated, was_truncated) = truncate_for_log(message, 4);

        assert!(was_truncated);
        assert_eq!(truncated, "abc→");
    }

    #[test]
    fn truncate_for_log_returns_original_when_within_limit() {
        let message = "hello";
        let (truncated, was_truncated) = truncate_for_log(message, 10);

        assert!(!was_truncated);
        assert_eq!(truncated, "hello");
    }
}
