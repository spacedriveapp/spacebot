//! Process daemonization and IPC for background operation.

use crate::config::{Config, TelemetryConfig};

use anyhow::{Context as _, anyhow};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_sdk::trace::SdkTracerProvider;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
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

    // Use the async-runtime-aware BatchSpanProcessor so the export future is
    // driven by tokio::spawn rather than a plain OS thread using
    // futures_executor::block_on. The sync variant panics because reqwest
    // calls tokio::time::sleep internally, which requires an active Tokio
    // runtime on the calling thread — something the plain thread never has.
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

/// Start the IPC server. Returns a shutdown receiver that the main event
/// loop should select on.
pub async fn start_ipc_server(
    paths: &DaemonPaths,
) -> anyhow::Result<(watch::Receiver<bool>, tokio::task::JoinHandle<()>)> {
    // Ensure the instance directory exists (e.g. on first run)
    if let Some(parent) = paths.socket.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create instance directory: {}", parent.display())
        })?;
    }

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
