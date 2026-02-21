use super::state::{ApiEvent, ApiState};

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Sse;
use futures::stream::Stream;
use serde::Serialize;
use std::convert::Infallible;
use std::io::Write as _;
use std::path::Component;
use std::path::Path;
use std::sync::Arc;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
pub(super) struct IdleResponse {
    idle: bool,
    active_workers: usize,
    active_branches: usize,
}

#[derive(Serialize)]
pub(super) struct StatusResponse {
    status: &'static str,
    version: &'static str,
    pid: u32,
    uptime_seconds: u64,
}

pub(super) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// Reports whether the instance is idle (no active workers or branches).
/// Used by the platform to gate rolling updates.
pub(super) async fn idle(State(state): State<Arc<ApiState>>) -> Json<IdleResponse> {
    let blocks = state.channel_status_blocks.read().await;
    let mut total_workers = 0;
    let mut total_branches = 0;

    for status_block in blocks.values() {
        let block = status_block.read().await;
        total_workers += block.active_workers.len();
        total_branches += block.active_branches.len();
    }

    Json(IdleResponse {
        idle: total_workers == 0 && total_branches == 0,
        active_workers: total_workers,
        active_branches: total_branches,
    })
}

pub(super) async fn status(State(state): State<Arc<ApiState>>) -> Json<StatusResponse> {
    let uptime = state.started_at.elapsed();
    Json(StatusResponse {
        status: "running",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        uptime_seconds: uptime.as_secs(),
    })
}

/// SSE endpoint streaming all agent events to connected clients.
pub(super) async fn events_sse(
    State(state): State<Arc<ApiState>>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>> {
    let mut rx = state.event_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let event_type = match &event {
                            ApiEvent::InboundMessage { .. } => "inbound_message",
                            ApiEvent::OutboundMessage { .. } => "outbound_message",
                            ApiEvent::TypingState { .. } => "typing_state",
                            ApiEvent::WorkerStarted { .. } => "worker_started",
                            ApiEvent::WorkerStatusUpdate { .. } => "worker_status",
                            ApiEvent::WorkerCompleted { .. } => "worker_completed",
                            ApiEvent::BranchStarted { .. } => "branch_started",
                            ApiEvent::BranchCompleted { .. } => "branch_completed",
                            ApiEvent::ToolStarted { .. } => "tool_started",
                            ApiEvent::ToolCompleted { .. } => "tool_completed",
                            ApiEvent::ConfigReloaded => "config_reloaded",
                        };
                        yield Ok(axum::response::sse::Event::default()
                            .event(event_type)
                            .data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    tracing::debug!(count, "SSE client lagged");
                    yield Ok(axum::response::sse::Event::default()
                        .event("lagged")
                        .data(format!("{{\"skipped\":{count}}}")));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

#[derive(Serialize)]
pub struct StorageStatus {
    used_bytes: u64,
    total_bytes: u64,
    available_bytes: u64,
}

pub(super) async fn storage_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<StorageStatus>, (axum::http::StatusCode, String)> {
    let runtime_configs = state.runtime_configs.load();
    let Some(runtime_config) = runtime_configs.values().next() else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no runtime config available".to_string(),
        ));
    };

    let instance_dir = runtime_config.instance_dir.clone();
    let status = tokio::task::spawn_blocking(move || read_filesystem_usage(&instance_dir))
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("storage task failed: {error}"),
            )
        })
        .and_then(|result| {
            result.map_err(|error| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("storage inspection failed: {error}"),
                )
            })
        })?;

    Ok(Json(status))
}

fn read_filesystem_usage(path: &Path) -> anyhow::Result<StorageStatus> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let path_cstring = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;

    let result = unsafe { libc::statvfs(path_cstring.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(anyhow::anyhow!("statvfs call failed"));
    }

    let stats = unsafe { stats.assume_init() };
    let block_size = stats.f_frsize as u128;
    let total_blocks = stats.f_blocks as u128;
    let avail_blocks = stats.f_bavail as u128;

    let total_bytes = (block_size * total_blocks) as u64;
    let used_bytes = directory_size_bytes(path)?;
    let available_bytes = (block_size * avail_blocks) as u64;

    Ok(StorageStatus {
        used_bytes,
        total_bytes,
        available_bytes,
    })
}

fn directory_size_bytes(root: &Path) -> anyhow::Result<u64> {
    if !root.exists() {
        return Ok(0);
    }

    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];

    while let Some(path) = stack.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;

        if metadata.file_type().is_symlink() {
            continue;
        }

        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            continue;
        }

        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                stack.push(entry.path());
            }
        }
    }

    Ok(total)
}

pub(super) async fn backup_export(
    State(state): State<Arc<ApiState>>,
) -> Result<impl IntoResponse, (axum::http::StatusCode, String)> {
    let runtime_configs = state.runtime_configs.load();
    let Some(runtime_config) = runtime_configs.values().next() else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no runtime config available".to_string(),
        ));
    };

    let instance_dir = runtime_config.instance_dir.clone();
    let archive_bytes = tokio::task::spawn_blocking(move || build_backup_zip(&instance_dir))
        .await
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("backup task failed: {error}"),
            )
        })
        .and_then(|result| {
            result.map_err(|error| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("backup generation failed: {error}"),
                )
            })
        })?;

    let headers = [
        (header::CONTENT_TYPE, "application/zip"),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=spacebot-backup.zip",
        ),
    ];

    Ok((headers, archive_bytes))
}

pub(super) async fn backup_restore(
    State(state): State<Arc<ApiState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    if body.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "backup archive payload is empty".to_string(),
        ));
    }

    let runtime_configs = state.runtime_configs.load();
    let Some(runtime_config) = runtime_configs.values().next() else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no runtime config available".to_string(),
        ));
    };

    let instance_dir = runtime_config.instance_dir.clone();
    let archive = body.to_vec();

    let restore_report =
        tokio::task::spawn_blocking(move || restore_backup_zip(&instance_dir, archive))
            .await
            .map_err(|error| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("restore task failed: {error}"),
                )
            })
            .and_then(|result| {
                result.map_err(|error| {
                    (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        format!("restore failed: {error}"),
                    )
                })
            })?;

    Ok(Json(serde_json::json!({
        "restored": true,
        "files_restored": restore_report.files_restored,
        "message": "backup restored to disk; restart instance to fully apply"
    })))
}

struct RestoreReport {
    files_restored: usize,
}

fn build_backup_zip(instance_dir: &Path) -> anyhow::Result<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let include_paths = [
        ("config.toml", instance_dir.join("config.toml")),
        ("agents", instance_dir.join("agents")),
    ];

    for (name, path) in include_paths {
        if !path.exists() {
            continue;
        }

        if path.is_file() {
            add_file_to_zip(&mut writer, &path, name, options)?;
        } else {
            add_directory_to_zip(&mut writer, &path, name, options)?;
        }
    }

    writer.finish()?;
    Ok(cursor.into_inner())
}

fn restore_backup_zip(
    instance_dir: &Path,
    archive_bytes: Vec<u8>,
) -> anyhow::Result<RestoreReport> {
    let restore_root = instance_dir.join(format!(".restore-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&restore_root)?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(archive_bytes))?;
    let mut files_restored = 0usize;

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(enclosed_name) = file.enclosed_name().map(|path| path.to_path_buf()) else {
            continue;
        };

        let allowed = enclosed_name == Path::new("config.toml")
            || enclosed_name.starts_with(Path::new("agents"));
        if !allowed {
            continue;
        }

        let target = restore_root.join(&enclosed_name);
        if file.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut output = std::fs::File::create(&target)?;
        std::io::copy(&mut file, &mut output)?;
        files_restored += 1;
    }

    let restored_config = restore_root.join("config.toml");
    if restored_config.exists() {
        replace_path_atomic(&restored_config, &instance_dir.join("config.toml"))?;
    }

    let restored_agents = restore_root.join("agents");
    if restored_agents.exists() {
        replace_directory(&restored_agents, &instance_dir.join("agents"))?;
    }

    let _ = std::fs::remove_dir_all(&restore_root);
    Ok(RestoreReport { files_restored })
}

fn replace_path_atomic(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let temp_destination = destination.with_extension("restore_tmp");
    std::fs::copy(source, &temp_destination)?;
    std::fs::rename(temp_destination, destination)?;
    Ok(())
}

fn replace_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let temp_destination = destination.with_extension("restore_tmp_dir");
    if temp_destination.exists() {
        std::fs::remove_dir_all(&temp_destination)?;
    }

    std::fs::rename(source, &temp_destination)?;

    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }

    std::fs::rename(&temp_destination, destination)?;
    Ok(())
}

fn add_directory_to_zip(
    writer: &mut zip::ZipWriter<&mut std::io::Cursor<Vec<u8>>>,
    directory_path: &Path,
    archive_prefix: &str,
    options: SimpleFileOptions,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory_path)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() && file_name == "logs" {
            continue;
        }

        if path.is_dir() && file_name == "workspace" {
            let name = format!("{archive_prefix}/{file_name}");
            add_workspace_directory_to_zip(writer, &path, &name, options)?;
            continue;
        }

        let name = format!("{archive_prefix}/{file_name}");

        if path.is_dir() {
            add_directory_to_zip(writer, &path, &name, options)?;
        } else if path.is_file() {
            add_file_to_zip(writer, &path, &name, options)?;
        }
    }

    Ok(())
}

fn add_workspace_directory_to_zip(
    writer: &mut zip::ZipWriter<&mut std::io::Cursor<Vec<u8>>>,
    workspace_path: &Path,
    archive_prefix: &str,
    options: SimpleFileOptions,
) -> anyhow::Result<()> {
    let walk = ignore::WalkBuilder::new(workspace_path)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build();

    for result in walk {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let path = entry.path();
        if path == workspace_path {
            continue;
        }

        if path
            .components()
            .any(|component| matches!(component, Component::Normal(name) if name == ".git"))
        {
            continue;
        }

        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if !metadata.is_file() {
            continue;
        }

        let Ok(relative) = path.strip_prefix(workspace_path) else {
            continue;
        };

        let relative_name = relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let archive_name = format!("{archive_prefix}/{relative_name}");
        add_file_to_zip(writer, path, &archive_name, options)?;
    }

    Ok(())
}

fn add_file_to_zip(
    writer: &mut zip::ZipWriter<&mut std::io::Cursor<Vec<u8>>>,
    file_path: &Path,
    archive_name: &str,
    options: SimpleFileOptions,
) -> anyhow::Result<()> {
    writer.start_file(archive_name, options)?;
    let file_bytes = std::fs::read(file_path)?;
    writer.write_all(&file_bytes)?;
    Ok(())
}
