//! Spacebot CLI entry point.

mod cli;

use anyhow::Context as _;
use arc_swap::ArcSwap;
use clap::Parser as _;
use cli::{Cli, Command};
use futures::StreamExt as _;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Tracks an active conversation channel and its message relay.
struct ActiveChannel {
    /// Non-blocking, order-preserving path into the channel's message queue.
    /// The router must never await channel delivery: a saturated channel
    /// would stall inbound routing for every other conversation.
    relay: spacebot::agent::inbound_relay::InboundRelay,
    /// Retained so the outbound routing task stays alive.
    _outbound_handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ActiveChannelKey {
    agent_id: String,
    conversation_id: String,
}

impl ActiveChannelKey {
    fn new(agent_id: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            conversation_id: conversation_id.into(),
        }
    }
}

/// Maximum number of deferred messages per channel before oldest are dropped.
const DEFERRED_INJECTION_CAP: usize = 64;

async fn load_worker_restoration_settings(
    pool: &sqlx::SqlitePool,
    agent_id: &str,
    conversation_id: &str,
) -> spacebot::conversation::settings::ResolvedConversationSettings {
    let portal_store = spacebot::conversation::PortalConversationStore::new(pool.clone());
    let channel_store = spacebot::conversation::ChannelSettingsStore::new(pool.clone());
    match portal_store.get(agent_id, conversation_id).await {
        Ok(Some(conversation)) => {
            spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                conversation.settings.as_ref(),
                None,
                None,
            )
        }
        Ok(None) => match channel_store.get(agent_id, conversation_id).await {
            Ok(Some(settings)) => {
                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                    Some(&settings),
                    None,
                    None,
                )
            }
            Ok(None) => Default::default(),
            Err(error) => {
                tracing::warn!(%error, %conversation_id, "idle worker restoration failed to load channel settings");
                Default::default()
            }
        },
        Err(error) => {
            tracing::warn!(%error, %conversation_id, "idle worker restoration failed to load portal settings");
            Default::default()
        }
    }
}

fn queue_deferred_injection(
    deferred_injections: &mut HashMap<ActiveChannelKey, Vec<spacebot::InboundMessage>>,
    injection: spacebot::ChannelInjection,
) {
    let key = ActiveChannelKey::new(injection.agent_id, injection.conversation_id);
    let queue = deferred_injections.entry(key).or_default();
    if queue.len() >= DEFERRED_INJECTION_CAP {
        tracing::warn!(
            "deferred injection queue at capacity ({DEFERRED_INJECTION_CAP}), dropping oldest message"
        );
        queue.remove(0);
    }
    queue.push(injection.message);
}

#[derive(Debug, serde::Serialize)]
struct BackfillTranscriptEntry {
    role: String,
    author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp_utc: Option<String>,
    content: String,
}

fn serialize_backfill_transcript(entries: Vec<BackfillTranscriptEntry>) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    match serde_json::to_string_pretty(&entries) {
        Ok(serialized) => Some(serialized),
        Err(error) => {
            tracing::warn!(%error, "failed to serialize backfill transcript");
            None
        }
    }
}

fn render_platform_history_backfill(
    history_messages: &[spacebot::messaging::traits::HistoryMessage],
) -> Option<String> {
    let entries = history_messages
        .iter()
        .map(|entry| BackfillTranscriptEntry {
            role: if entry.is_bot {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            author: if entry.is_bot {
                "(you)".to_string()
            } else {
                entry.author.clone()
            },
            timestamp_utc: entry
                .timestamp
                .as_ref()
                .map(|timestamp| timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            content: entry.content.clone(),
        })
        .collect();

    serialize_backfill_transcript(entries)
}

/// Forward outbound response events to SSE clients for the dashboard.
fn forward_sse_event(
    api_event_tx: &tokio::sync::broadcast::Sender<spacebot::api::ApiEvent>,
    agent_id: &str,
    channel_id: &str,
    response: &spacebot::OutboundResponse,
) {
    match response {
        spacebot::OutboundResponse::Text(text)
        | spacebot::OutboundResponse::RichMessage { text, .. }
        | spacebot::OutboundResponse::ThreadReply { text, .. } => {
            api_event_tx
                .send(spacebot::api::ApiEvent::OutboundMessage {
                    agent_id: agent_id.to_string(),
                    channel_id: channel_id.to_string(),
                    text: text.clone(),
                })
                .ok();
        }
        spacebot::OutboundResponse::Status(spacebot::StatusUpdate::Thinking) => {
            api_event_tx
                .send(spacebot::api::ApiEvent::TypingState {
                    agent_id: agent_id.to_string(),
                    channel_id: channel_id.to_string(),
                    is_typing: true,
                })
                .ok();
        }
        spacebot::OutboundResponse::Status(spacebot::StatusUpdate::StopTyping) => {
            api_event_tx
                .send(spacebot::api::ApiEvent::TypingState {
                    agent_id: agent_id.to_string(),
                    channel_id: channel_id.to_string(),
                    is_typing: false,
                })
                .ok();
        }
        // Portal has no ephemeral surface; command replies render as
        // ordinary messages instead of disappearing.
        spacebot::OutboundResponse::Ephemeral { text, .. } => {
            api_event_tx
                .send(spacebot::api::ApiEvent::OutboundMessage {
                    agent_id: agent_id.to_string(),
                    channel_id: channel_id.to_string(),
                    text: text.clone(),
                })
                .ok();
        }
        _ => {}
    }
}

/// Route an outbound response to the messaging adapter using the pinned target
/// message for platform routing metadata (thread_ts, channel_id, etc.).
async fn route_outbound(
    messaging: &std::sync::Arc<spacebot::messaging::MessagingManager>,
    target: &spacebot::InboundMessage,
    response: spacebot::OutboundResponse,
) -> Result<(), String> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        match response {
            spacebot::OutboundResponse::Status(status) => messaging
                .send_status(target, status)
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "failed to send status update");
                    error.to_string()
                }),
            response => messaging.respond(target, response).await.map_err(|error| {
                tracing::error!(%error, "failed to send outbound response");
                error.to_string()
            }),
        }
    })
    .await
    .unwrap_or_else(|_| Err("messaging adapter delivery timed out".to_string()))
}

fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Start { foreground: false });

    match command {
        Command::Start { foreground } => {
            let restart_spec = spacebot::lifecycle::RestartSpec::capture(
                cli.config.as_deref(),
                cli.debug,
                foreground,
            );
            cmd_start(cli.config, cli.debug, foreground, restart_spec)
        }
        Command::Stop => cmd_stop(),
        Command::Restart { foreground } => cmd_restart(cli.config, cli.debug, foreground),
        Command::Status => cmd_status(),
        command => cli::dispatch(
            command,
            cli::Context {
                config_path: cli.config,
                json: cli.json,
                url: cli.url,
                token: cli.token,
            },
        ),
    }
}

fn cmd_start(
    config_path: Option<std::path::PathBuf>,
    debug: bool,
    foreground: bool,
    restart_spec: spacebot::lifecycle::RestartSpec,
) -> anyhow::Result<()> {
    // Use the config path (if provided) to derive the correct instance dir
    // for the PID check, so it matches the PID file written during daemonize.
    let instance_dir = resolve_instance_dir(&config_path);
    let paths = spacebot::daemon::DaemonPaths::new(&instance_dir);

    // Bail if already running
    if let Some(pid) = spacebot::daemon::is_running(&paths) {
        eprintln!("spacebot is already running (pid {pid})");
        std::process::exit(1);
    }

    // Run onboarding interactively before daemonizing. Skipped after a
    // self-restart re-exec: stdin is /dev/null there, so the wizard could
    // never complete — fall through to setup mode instead.
    let resolved_config_path = if config_path.is_some() {
        config_path.clone()
    } else if std::env::var_os("SPACEBOT_REEXEC").is_none()
        && spacebot::config::Config::needs_onboarding()
    {
        // Returns Some(path) if CLI wizard ran, None if user chose the UI.
        spacebot::config::run_onboarding().with_context(|| "onboarding failed")?
    } else {
        None
    };

    if !foreground {
        // Fork BEFORE touching the macOS Keychain or any CoreFoundation API.
        //
        // bootstrap_secrets_store() loads the master key from the macOS Keychain
        // (Security framework), which initializes CoreFoundation internally.
        // On macOS, CoreFoundation state is not safe to use after fork() — the
        // child process receives SIGBUS from the kernel. To avoid this, we
        // determine the instance directory (needed for the PID file path)
        // without loading the full config or accessing the Keychain, then fork
        // first. Config loading and secrets resolution happen in the child.
        //
        // Tokio's I/O driver and thread pool also don't survive fork, so the
        // runtime and tracing init must happen after this call as well.
        spacebot::daemon::daemonize(&paths)?;
    }

    // Open the instance-level secrets store so `secret:` references in config.toml
    // resolve during Config::load(). Now safe to access the macOS Keychain —
    // we are either in foreground mode (no fork) or in the daemon child process.
    let bootstrapped_store = bootstrap_secrets_store(&resolved_config_path);

    let config = cli::load_config(&resolved_config_path)?;

    // Build a fresh Tokio runtime in this process (the child after daemonize,
    // or the foreground process). Tracing init — including the OTLP batch
    // exporter — must happen inside block_on because the async
    // BatchSpanProcessor calls tokio::spawn at construction time and requires
    // an active runtime handle.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build Tokio runtime")?;

    runtime.block_on(async {
        let otel_provider = if foreground {
            spacebot::daemon::init_foreground_tracing(debug, &config.telemetry)
        } else {
            let paths = spacebot::daemon::DaemonPaths::new(&config.instance_dir);
            spacebot::daemon::init_background_tracing(&paths, debug, &config.telemetry)
        };

        run(
            config,
            foreground,
            otel_provider,
            bootstrapped_store,
            restart_spec,
        )
        .await
    })
}

/// Restart the daemon. When it is running and supports in-place restart, ask
/// it to re-exec itself over IPC and confirm via the run_id nonce; otherwise
/// fall back to stop-then-start.
fn cmd_restart(
    config_path: Option<std::path::PathBuf>,
    debug: bool,
    foreground: bool,
) -> anyhow::Result<()> {
    let instance_dir = resolve_instance_dir(&config_path);
    let paths = spacebot::daemon::DaemonPaths::new(&instance_dir);

    // A foreground restart must attach the daemon to this terminal, which an
    // in-place re-exec of the already-daemonized process cannot do — use the
    // stop-then-start path for that, and when nothing is running yet.
    if foreground || spacebot::daemon::is_running(&paths).is_none() {
        return stop_then_start(config_path, debug, foreground, &paths);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    let outcome = runtime.block_on(request_daemon_restart(&paths));
    drop(runtime);

    match outcome {
        RestartOutcome::Confirmed { pid } => {
            eprintln!("spacebot restarted (pid {pid})");
            Ok(())
        }
        RestartOutcome::Rejected { message } => {
            eprintln!("restart rejected: {message}");
            std::process::exit(1);
        }
        RestartOutcome::Unconfirmed => {
            eprintln!("restart requested, but no restarted daemon responded within 30 seconds");
            eprintln!("check `spacebot status`");
            std::process::exit(1);
        }
        RestartOutcome::Unsupported => {
            // The running daemon predates the in-place restart command.
            stop_then_start(config_path, debug, foreground, &paths)
        }
    }
}

/// Stop any running daemon, then start fresh. The fallback restart path when
/// an in-place re-exec is not possible.
fn stop_then_start(
    config_path: Option<std::path::PathBuf>,
    debug: bool,
    foreground: bool,
    paths: &spacebot::daemon::DaemonPaths,
) -> anyhow::Result<()> {
    cmd_stop_if_running(paths);
    let restart_spec =
        spacebot::lifecycle::RestartSpec::capture(config_path.as_deref(), debug, foreground);
    cmd_start(config_path, debug, foreground, restart_spec)
}

enum RestartOutcome {
    Confirmed { pid: u32 },
    Rejected { message: String },
    Unconfirmed,
    Unsupported,
}

async fn request_daemon_restart(paths: &spacebot::daemon::DaemonPaths) -> RestartOutcome {
    use spacebot::daemon::{IpcCommand, IpcResponse, send_command};

    let old_run_id = match send_command(paths, IpcCommand::Status).await {
        Ok(IpcResponse::Status { run_id, .. }) => run_id,
        _ => None,
    };

    match send_command(paths, IpcCommand::Restart).await {
        Ok(IpcResponse::Ok) => {}
        Ok(IpcResponse::Error { message }) => return RestartOutcome::Rejected { message },
        // An older daemon fails to parse the command and drops the connection
        // without responding.
        _ => return RestartOutcome::Unsupported,
    }

    eprintln!("restarting spacebot...");

    // The daemon tears down after a grace delay, then re-execs (foreground)
    // or re-daemonizes (background). A foreground re-exec keeps its PID, so
    // the run_id nonce is the only reliable confirmation. Connection failures
    // are the gap between the old daemon unbinding and the new one binding.
    for _ in 0..120 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Ok(IpcResponse::Status { pid, run_id, .. }) =
            send_command(paths, IpcCommand::Status).await
            && run_id != old_run_id
        {
            return RestartOutcome::Confirmed { pid };
        }
    }
    RestartOutcome::Unconfirmed
}

/// Resolve the instance directory from the config path without loading the
/// full config or touching platform credential stores. Used to determine
/// daemon file paths (PID, socket) before fork.
fn resolve_instance_dir(config_path: &Option<std::path::PathBuf>) -> std::path::PathBuf {
    if let Some(path) = config_path {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    } else {
        spacebot::config::Config::default_instance_dir()
    }
}

#[tokio::main]
async fn cmd_stop() -> anyhow::Result<()> {
    let paths = spacebot::daemon::DaemonPaths::from_default();

    let Some(pid) = spacebot::daemon::is_running(&paths) else {
        eprintln!("spacebot is not running");
        std::process::exit(1);
    };

    match spacebot::daemon::send_command(&paths, spacebot::daemon::IpcCommand::Shutdown).await {
        Ok(spacebot::daemon::IpcResponse::Ok) => {
            eprintln!("stopping spacebot (pid {pid})...");
        }
        Ok(spacebot::daemon::IpcResponse::Error { message }) => {
            eprintln!("shutdown failed: {message}");
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("unexpected response from daemon");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("failed to send shutdown command: {error}");
            std::process::exit(1);
        }
    }

    if spacebot::daemon::wait_for_exit(pid) {
        eprintln!("spacebot stopped");
    } else {
        eprintln!("spacebot did not stop within 10 seconds (pid {pid})");
        std::process::exit(1);
    }

    Ok(())
}

/// Stop if running, don't error if not.
fn cmd_stop_if_running(paths: &spacebot::daemon::DaemonPaths) {
    let Some(pid) = spacebot::daemon::is_running(paths) else {
        return;
    };

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };

    runtime.block_on(async {
        if let Ok(spacebot::daemon::IpcResponse::Ok) =
            spacebot::daemon::send_command(paths, spacebot::daemon::IpcCommand::Shutdown).await
        {
            eprintln!("stopping spacebot (pid {pid})...");
            spacebot::daemon::wait_for_exit(pid);
        }
    });
}

fn cmd_status() -> anyhow::Result<()> {
    let paths = spacebot::daemon::DaemonPaths::from_default();

    let Some(_pid) = spacebot::daemon::is_running(&paths) else {
        eprintln!("spacebot is not running");
        std::process::exit(1);
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async {
        match spacebot::daemon::send_command(&paths, spacebot::daemon::IpcCommand::Status).await {
            Ok(spacebot::daemon::IpcResponse::Status {
                pid,
                uptime_seconds,
                ..
            }) => {
                let hours = uptime_seconds / 3600;
                let minutes = (uptime_seconds % 3600) / 60;
                let seconds = uptime_seconds % 60;
                eprintln!("spacebot is running");
                eprintln!("  pid:    {pid}");
                eprintln!("  uptime: {hours}h {minutes}m {seconds}s");
            }
            Ok(spacebot::daemon::IpcResponse::Error { message }) => {
                eprintln!("status query failed: {message}");
                std::process::exit(1);
            }
            Ok(_) => {
                eprintln!("unexpected response from daemon");
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!("failed to query daemon status: {error}");
                std::process::exit(1);
            }
        }
    });

    Ok(())
}

/// Pre-open secrets stores before config loading so `secret:` references in
/// config.toml can resolve.
///
/// Config resolution happens in `Config::load()`, which calls `resolve_env_value()`
/// for every credential field. That function checks the thread-local
/// `RESOLVE_SECRETS_STORE` for `secret:` prefixed values. Without this bootstrap,
/// all `secret:` references resolve to `None` and the config fails validation
/// (e.g., messaging adapters see empty tokens and error out).
///
/// Returns the pre-opened stores keyed by agent ID. These are reused later in
/// `initialize_agents()` to avoid double-opening the redb files.
/// Keystore identifier for the instance-level master key.
const KEYSTORE_INSTANCE_ID: &str = "instance";

/// Open the instance-level secrets store at `<instance_dir>/data/secrets.redb`
/// before config loading so that `secret:` references in config.toml resolve.
///
/// If no instance-level store exists but per-agent stores do (from the previous
/// per-agent model), secrets are migrated from the first non-empty agent store.
fn bootstrap_secrets_store(
    config_path: &Option<std::path::PathBuf>,
) -> Option<Arc<spacebot::secrets::store::SecretsStore>> {
    // Probe kernel keyring support before any workers spawn. If keyctl is
    // blocked (restrictive seccomp, gVisor, etc.), worker keyring isolation
    // is disabled but workers still start normally.
    spacebot::secrets::keystore::probe_keyring_support();

    let instance_dir = resolve_instance_dir(config_path);

    let data_dir = instance_dir.join("data");
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("warning: failed to create instance data directory: {error}");
        return None;
    }

    let secrets_path = data_dir.join("secrets.redb");
    let is_new_store = !secrets_path.exists();

    let store = match spacebot::secrets::store::SecretsStore::new(&secrets_path) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            eprintln!("warning: failed to open secrets store: {error}");
            return None;
        }
    };

    // Migrate from legacy per-agent stores if the instance store is brand new.
    if is_new_store {
        migrate_legacy_agent_stores(&instance_dir, &store);
    }

    // Try to auto-unlock if encrypted.
    if store.is_encrypted() {
        let keystore = spacebot::secrets::keystore::platform_keystore();
        let tmpfs_paths = [
            std::path::Path::new("/run/spacebot/master_key"),
            std::path::Path::new("/run/secrets/master_key"),
        ];

        // Hosted: check tmpfs-injected key.
        let tmpfs_master_key = tmpfs_paths.iter().find_map(|path| {
            if !path.exists() {
                return None;
            }

            let raw_key = match std::fs::read(path) {
                Ok(key) => key,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "failed to read tmpfs master key");
                    return None;
                }
            };

            // Platform currently stores keys as 64-char hex strings. Decode
            // those to raw bytes before unlock; otherwise treat as raw bytes.
            if let Ok(text) = std::str::from_utf8(&raw_key) {
                let trimmed = text.trim();
                if trimmed.len() == 64 && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return match hex::decode(trimmed) {
                        Ok(decoded) => Some(decoded),
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                path = %path.display(),
                                "failed to decode hex tmpfs master key, falling back to raw bytes"
                            );
                            Some(raw_key)
                        }
                    };
                }
            }

            Some(raw_key)
        });

        let mut unlocked = false;

        if let Some(key) = tmpfs_master_key {
            match store.unlock(&key) {
                Ok(()) => {
                    unlocked = true;
                    if let Err(error) = keystore.store_key(KEYSTORE_INSTANCE_ID, &key) {
                        tracing::warn!(%error, "failed to persist master key to OS credential store");
                    }
                    // Clean up tmpfs key files only after a successful unlock.
                    for cleanup_path in tmpfs_paths {
                        if cleanup_path.exists()
                            && let Err(error) = std::fs::remove_file(cleanup_path)
                        {
                            tracing::warn!(
                                %error,
                                path = %cleanup_path.display(),
                                "failed to remove tmpfs master key — key may remain accessible"
                            );
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to unlock secret store with tmpfs key");
                }
            }
        }

        if !unlocked {
            // Try instance-level key first, then fall back to legacy agent keys.
            let master_key = keystore
                .load_key(KEYSTORE_INSTANCE_ID)
                .ok()
                .flatten()
                .or_else(|| load_legacy_keystore_key(&instance_dir));

            if let Some(key) = master_key
                && let Err(error) = store.unlock(&key)
            {
                tracing::warn!(
                    %error,
                    "failed to unlock secret store — secrets will be inaccessible"
                );
            }
        }
    }

    // Set the store into the thread-local for config resolution.
    spacebot::config::set_resolve_secrets_store(store.clone());

    Some(store)
}

/// Migrate secrets from legacy per-agent redb stores into the new instance-level
/// store. Only runs once when the instance-level store is first created.
fn migrate_legacy_agent_stores(
    instance_dir: &std::path::Path,
    target_store: &spacebot::secrets::store::SecretsStore,
) {
    let agents_dir = instance_dir.join("agents");
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut total_migrated = 0usize;

    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let secrets_path = entry.path().join("data").join("secrets.redb");
        if !secrets_path.exists() {
            continue;
        }

        // Open the legacy agent store (read-only access).
        let legacy_store = match spacebot::secrets::store::SecretsStore::new(&secrets_path) {
            Ok(store) => store,
            Err(_) => continue,
        };

        // If the legacy store is encrypted, try to unlock it with OS keystore.
        if legacy_store.is_encrypted() {
            let agent_id = entry.file_name().to_string_lossy().to_string();
            let keystore = spacebot::secrets::keystore::platform_keystore();
            if let Some(key) = keystore.load_key(&agent_id).ok().flatten() {
                let _ = legacy_store.unlock(&key);
            } else {
                continue; // Can't read encrypted store without key.
            }
        }

        // Export all secrets from the legacy store.
        let export = match legacy_store.export_all() {
            Ok(export) => export,
            Err(_) => continue,
        };

        // Import into the target store (don't overwrite — first agent wins for
        // duplicates, which is fine since all agents had the same secrets).
        match target_store.import_all(&export, false) {
            Ok(result) => {
                total_migrated += result.imported;
            }
            Err(error) => {
                eprintln!(
                    "warning: failed to migrate secrets from {}: {error}",
                    secrets_path.display()
                );
            }
        }
    }

    if total_migrated > 0 {
        eprintln!(
            "info: migrated {total_migrated} secrets from legacy per-agent stores to instance store"
        );
    }
}

/// Try to load a master key from legacy per-agent keystore entries.
fn load_legacy_keystore_key(instance_dir: &std::path::Path) -> Option<Vec<u8>> {
    let agents_dir = instance_dir.join("agents");
    let entries = std::fs::read_dir(&agents_dir).ok()?;
    let keystore = spacebot::secrets::keystore::platform_keystore();

    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let agent_id = entry.file_name().to_string_lossy().to_string();
        if let Ok(Some(key)) = keystore.load_key(&agent_id) {
            // Migrate the key to the instance-level keystore entry.
            let _ = keystore.store_key(KEYSTORE_INSTANCE_ID, &key);
            return Some(key);
        }
    }
    None
}

fn has_provider_credentials(
    llm_config: &spacebot::config::LlmConfig,
    instance_dir: &std::path::Path,
) -> bool {
    llm_config.has_any_key()
        || spacebot::auth::credentials_path(instance_dir).exists()
        || spacebot::openai_auth::credentials_path(instance_dir).exists()
}

fn configured_agent_infos(config: &spacebot::config::Config) -> Vec<spacebot::api::AgentInfo> {
    config
        .resolve_agents()
        .into_iter()
        .map(|agent| spacebot::api::AgentInfo {
            id: agent.id,
            display_name: agent.display_name,
            role: agent.role,
            gradient_start: agent.gradient_start,
            gradient_end: agent.gradient_end,
            workspace: agent.workspace.to_string_lossy().to_string(),
            context_window: agent.context_window,
            max_turns: agent.max_turns,
            max_concurrent_branches: agent.max_concurrent_branches,
            max_concurrent_workers: agent.max_concurrent_workers,
        })
        .collect()
}

async fn run(
    config: spacebot::config::Config,
    foreground: bool,
    otel_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    bootstrapped_store: Option<Arc<spacebot::secrets::store::SecretsStore>>,
    restart_spec: spacebot::lifecycle::RestartSpec,
) -> anyhow::Result<()> {
    let paths = spacebot::daemon::DaemonPaths::new(&config.instance_dir);

    tracing::info!("starting spacebot");
    tracing::info!(instance_dir = %config.instance_dir.display(), "configuration loaded");

    // SIGTERM stream for orchestrated stops (docker stop, systemctl stop).
    // Installed before agent initialization: that window includes database
    // migrations and model warmup, and a SIGTERM under the default disposition
    // would kill the process with no teardown at all. Signals arriving during
    // startup are buffered by the stream and picked up by the main loop.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to install SIGTERM handler")?;

    // Start the IPC server for stop/restart/status commands
    let (lifecycle, mut shutdown_rx) = spacebot::lifecycle::LifecycleHandle::new();
    let run_id = uuid::Uuid::new_v4().to_string();
    let _ipc_handle = spacebot::daemon::start_ipc_server(&paths, lifecycle.clone(), run_id)
        .await
        .context("failed to start IPC server")?;

    // Create the provider setup channel so API handlers can signal the main loop
    let (provider_tx, mut provider_rx) = mpsc::channel::<spacebot::ProviderSetupEvent>(1);
    // Channel for newly created agents to be registered in the main event loop
    let (agent_tx, mut agent_rx) = mpsc::channel::<spacebot::Agent>(8);
    // Channel for removing agents from the main event loop
    let (agent_remove_tx, mut agent_remove_rx) = mpsc::channel::<String>(8);

    // Channel for cross-agent message injection (e.g. delegated task completion notifications).
    // The sender is shared with all agents via AgentDeps; the receiver is polled in the main loop.
    let (injection_tx, mut injection_rx) =
        tokio::sync::mpsc::channel::<spacebot::ChannelInjection>(64);

    // Instance-level global task database. Shared across all agents with globally
    // unique task numbers. Lives alongside secrets.redb in the instance data dir.
    let instance_pool = spacebot::db::connect_instance_db(&config.instance_dir.join("data"))
        .await
        .context("failed to initialize instance database")?;

    // Migrate legacy per-agent tasks to the global database on first run.
    spacebot::tasks::migration::migrate_legacy_tasks(&config.instance_dir, &instance_pool)
        .await
        .context("failed to migrate legacy tasks to global database")?;

    let global_task_store = Arc::new(spacebot::tasks::TaskStore::new(instance_pool.clone()));

    // Tasks that predate revision history get a baseline snapshot of exactly
    // how they stand now. Idempotent, so a retry after a partial run is a
    // no-op; it does not reconstruct descriptions that were overwritten before
    // history existed.
    let baselines = global_task_store
        .backfill_baseline_revisions()
        .await
        .context("failed to write baseline task revisions")?;
    if baselines > 0 {
        tracing::info!(tasks = baselines, "wrote baseline task revisions");
    }

    // Instance-level goal store. Goals are instance-scoped like tasks.
    let global_goal_store = Arc::new(spacebot::goals::GoalStore::new(instance_pool.clone()));

    // Instance-wide wiki knowledge base.
    let global_wiki_store = Arc::new(spacebot::wiki::WikiStore::new(instance_pool.clone()));

    // Instance-level notification store for the dashboard inbox.
    let global_notification_store = Arc::new(spacebot::notifications::NotificationStore::new(
        instance_pool.clone(),
    ));

    // Instance-level shared project store. Replaces per-agent project stores.
    let global_project_store =
        Arc::new(spacebot::projects::ProjectStore::new(instance_pool.clone()));

    // Migrate per-agent projects into the instance database on first run.
    spacebot::projects::migration::migrate_legacy_projects(&config.instance_dir, &instance_pool)
        .await
        .context("failed to migrate legacy projects to instance database")?;

    // Tasks executed before the worktree binding was recorded have a
    // `task-<number>` worktree on disk that nothing points at. Reconnect them
    // by name so a retry reuses the worktree instead of rediscovering it.
    {
        let mut candidates = Vec::new();
        match global_project_store.list_projects(None).await {
            Ok(projects) => {
                for project in projects {
                    match global_project_store.list_worktrees(&project.id).await {
                        Ok(worktrees) => {
                            candidates.extend(worktrees.into_iter().map(|w| (w.name, w.id)))
                        }
                        Err(error) => {
                            tracing::warn!(
                                project_id = %project.id,
                                %error,
                                "failed to list worktrees for task binding backfill"
                            );
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to list projects for task binding backfill");
            }
        }

        match global_task_store
            .backfill_worktree_bindings(&candidates)
            .await
        {
            Ok(bound) if bound > 0 => {
                tracing::info!(tasks = bound, "bound tasks to their existing worktrees");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "failed to backfill task worktree bindings");
            }
        }
    }

    // Start HTTP API server if enabled
    let mut api_state = spacebot::api::ApiState::new_with_provider_sender(
        provider_tx,
        agent_tx,
        agent_remove_tx,
        injection_tx.clone(),
    );
    api_state.auth_token = config.api.auth_token.clone();
    // Instance-wide autonomy ceiling: one ArcSwap shared between the API and
    // every AgentDeps so ceiling writes take effect without a restart.
    api_state.autonomy_ceiling = Arc::new(arc_swap::ArcSwap::from_pointee(config.autonomy_ceiling));
    api_state.set_task_store(global_task_store.clone());
    api_state.set_goal_store(global_goal_store.clone());
    api_state.set_wiki_store(global_wiki_store.clone());
    api_state.set_notification_store(global_notification_store.clone());
    api_state.set_lifecycle(lifecycle.clone());
    let api_state = Arc::new(api_state);

    // Keep the secrets API available in setup mode so encrypted stores can be
    // unlocked before providers/agents are initialized.
    if let Some(store) = &bootstrapped_store {
        api_state.set_secrets_store(store.clone());
    }

    // Start background update checker
    spacebot::update::spawn_update_checker(api_state.update_status.clone());

    // Start metrics server if enabled (requires `metrics` cargo feature)
    #[cfg(feature = "metrics")]
    let _metrics_handle = if config.metrics.enabled {
        Some(
            spacebot::telemetry::start_metrics_server(&config.metrics, shutdown_rx.clone())
                .await
                .context("failed to start metrics server")?,
        )
    } else {
        None
    };

    let _http_handle = if config.api.enabled {
        // IPv6 addresses need brackets when combined with port: [::]:19898
        let raw_bind = config
            .api
            .bind
            .trim_start_matches('[')
            .trim_end_matches(']');
        let bind_str = if raw_bind.contains(':') {
            format!("[{}]:{}", raw_bind, config.api.port)
        } else {
            format!("{}:{}", raw_bind, config.api.port)
        };
        let bind: std::net::SocketAddr = bind_str.parse().context("invalid API bind address")?;
        let http_shutdown = shutdown_rx.clone();
        Some(
            spacebot::api::start_http_server(bind, api_state.clone(), http_shutdown)
                .await
                .context("failed to start HTTP server")?,
        )
    } else {
        None
    };

    // Check if we have provider configuration (API keys or OAuth credentials)
    let has_providers = has_provider_credentials(&config.llm, &config.instance_dir);

    if !has_providers {
        tracing::info!("No LLM providers configured. Starting in setup mode.");
        if foreground {
            eprintln!("No LLM provider keys configured.");
            eprintln!(
                "Please add a provider key via the web UI at http://{}:{}",
                config.api.bind, config.api.port
            );
        }
    }

    // Shared LLM manager (same API keys for all agents)
    // This works even without keys; it will fail later at call time if no keys exist.
    // Loads OAuth credentials from auth.json if available.
    let llm_manager = Arc::new(
        spacebot::llm::LlmManager::with_instance_dir(
            config.llm.clone(),
            config.instance_dir.clone(),
        )
        .await
        .with_context(|| "failed to initialize LLM manager")?,
    );

    // The hard ceiling every request is trimmed to fit. Compaction aims at the
    // same number, but it only runs where a loop yields; this is enforced on
    // the request itself, so a loop that never yields cannot exceed it. Raising
    // `context_window` raises both — set it to what the backend actually
    // enforces, which is not always what the model advertises.
    llm_manager.set_default_context_ceiling(config.defaults.context_window);

    // Shared embedding model (stateless, agent-agnostic)
    let embedding_cache_dir = config.instance_dir.join("embedding_cache");
    let embedding_model = Arc::new(
        spacebot::memory::EmbeddingModel::new(&embedding_cache_dir)
            .context("failed to initialize embedding model")?,
    );

    tracing::info!("shared resources initialized");

    // Initialize the language for all text lookups (must happen before PromptEngine/tools)
    spacebot::prompts::text::init("en").with_context(|| "failed to initialize language")?;

    // Create the PromptEngine with bundled templates (no file watching, no user overrides)
    let prompt_engine = spacebot::prompts::PromptEngine::new("en")
        .with_context(|| "failed to initialize prompt engine")?;

    // Parse config links into shared agent links (hot-reloadable via ArcSwap)
    let agent_links = Arc::new(ArcSwap::from_pointee(
        spacebot::links::AgentLink::from_config(&config.links)?,
    ));
    if !config.links.is_empty() {
        tracing::info!(count = config.links.len(), "loaded agent links from config");
    }

    // Shared humans list (hot-reloadable via ArcSwap, same pattern as agent_links)
    let agent_humans = Arc::new(ArcSwap::from_pointee(config.humans.clone()));

    // These hold the initialized subsystems. Empty until agents are initialized.
    let mut agents: HashMap<spacebot::AgentId, spacebot::Agent> = HashMap::new();
    let mut messaging_manager: Arc<spacebot::messaging::MessagingManager> =
        Arc::new(spacebot::messaging::MessagingManager::new());
    // Use an Option to represent "no inbound stream yet" (setup mode)
    let mut inbound_stream: Option<
        std::pin::Pin<Box<dyn futures::Stream<Item = spacebot::InboundMessage> + Send>>,
    > = None;
    let mut cron_schedulers_for_shutdown: Vec<Arc<spacebot::cron::Scheduler>> = Vec::new();
    let mut _ingestion_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut _cortex_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let bindings: Arc<ArcSwap<Vec<spacebot::config::Binding>>> =
        Arc::new(ArcSwap::from_pointee(config.bindings.clone()));
    api_state.set_bindings(bindings.clone()).await;
    let authority_defaults: Arc<ArcSwap<spacebot::commands::access::AdapterAuthorityDefaults>> =
        Arc::new(ArcSwap::from_pointee(
            spacebot::commands::access::AdapterAuthorityDefaults::from_config(&config),
        ));
    let default_agent_id = config.default_agent_id().to_string();

    // Set the config path on the API state for config.toml writes
    let config_path = config.instance_dir.join("config.toml");
    api_state.set_config_path(config_path.clone()).await;
    api_state.set_instance_dir(config.instance_dir.clone());
    api_state.set_llm_manager(llm_manager.clone()).await;
    api_state.set_embedding_model(embedding_model.clone()).await;
    api_state.set_prompt_engine(prompt_engine.clone()).await;
    api_state.set_defaults_config(config.defaults.clone()).await;
    api_state.set_agent_links((**agent_links.load()).clone());
    api_state.set_agent_groups(config.groups.clone());
    api_state.set_agent_humans(config.humans.clone());
    api_state.set_agent_configs(configured_agent_infos(&config));

    // Track whether agents have been initialized
    let mut agents_initialized = false;

    // File watcher handle — started after agent init (or in setup mode with empty data)
    let mut _file_watcher: Option<spacebot::config::FileWatcherHandle>;

    // If providers are available, initialize agents immediately
    if has_providers {
        let mut watcher_agents = Vec::new();
        let mut discord_permissions = None;
        let mut slack_permissions = None;
        let mut telegram_permissions = None;
        let mut twitch_permissions = None;
        let mut mattermost_permissions = None;
        let mut signal_permissions = None;
        initialize_agents(
            &config,
            &llm_manager,
            &embedding_model,
            &prompt_engine,
            &api_state,
            &mut agents,
            &mut messaging_manager,
            &mut inbound_stream,
            &mut cron_schedulers_for_shutdown,
            &mut _ingestion_handles,
            &mut _cortex_handles,
            &mut watcher_agents,
            &mut discord_permissions,
            &mut slack_permissions,
            &mut telegram_permissions,
            &mut twitch_permissions,
            &mut mattermost_permissions,
            &mut signal_permissions,
            agent_links.clone(),
            agent_humans.clone(),
            injection_tx.clone(),
            global_task_store.clone(),
            global_goal_store.clone(),
            global_wiki_store.clone(),
            global_project_store.clone(),
            global_notification_store.clone(),
            &bootstrapped_store,
        )
        .await?;
        agents_initialized = true;

        // Start file watcher with populated agent data
        _file_watcher = Some(spacebot::config::spawn_file_watcher(
            config_path.clone(),
            config.instance_dir.clone(),
            watcher_agents,
            discord_permissions,
            slack_permissions,
            telegram_permissions,
            twitch_permissions,
            mattermost_permissions,
            signal_permissions,
            bindings.clone(),
            authority_defaults.clone(),
            Some(messaging_manager.clone()),
            llm_manager.clone(),
            agent_links.clone(),
            agent_humans.clone(),
        ));
    } else {
        // Start file watcher in setup mode (no agents to watch yet)
        _file_watcher = Some(spacebot::config::spawn_file_watcher(
            config_path.clone(),
            config.instance_dir.clone(),
            Vec::new(),
            None, // discord_permissions
            None, // slack_permissions
            None, // telegram_permissions
            None, // twitch_permissions
            None, // mattermost_permissions
            None, // signal_permissions
            bindings.clone(),
            authority_defaults.clone(),
            None,
            llm_manager.clone(),
            agent_links.clone(),
            agent_humans.clone(),
        ));
    }

    if foreground {
        eprintln!(
            "spacebot running in foreground (pid {})",
            std::process::id()
        );
    } else {
        tracing::info!(pid = std::process::id(), "spacebot daemon started");
    }

    // Active conversation channels keyed by their owning agent and conversation.
    let mut active_channels: HashMap<ActiveChannelKey, ActiveChannel> = HashMap::new();
    let mut deferred_injections: HashMap<ActiveChannelKey, Vec<spacebot::InboundMessage>> =
        HashMap::new();

    // Workers run in-process, so any attempt still open belongs to a run that
    // died with the previous process. Close them, or the task-scoped spawn
    // guard would see a live run forever and that task could never be worked
    // again.
    //
    // A run can reach a terminal state and still leave its attempt open: the
    // worker record lives in the agent database and the attempt in the instance
    // one, so nothing spans both writes. Where the worker did commit an outcome
    // the attempt is closed with it, and only the runs nothing decided are swept
    // as interrupted. This runs after the agents are open because recovering an
    // outcome means reading the agent database that holds it.
    if agents_initialized {
        let live = match global_task_store.live_attempts().await {
            Ok(live) => live,
            Err(error) => {
                tracing::warn!(%error, "failed to read live task attempts");
                Vec::new()
            }
        };
        for attempt in live {
            let Some(agent) = attempt
                .agent_id
                .as_deref()
                .and_then(|id| agents.get(&spacebot::AgentId::from(id)))
            else {
                continue;
            };
            let Ok(worker_id) = attempt.worker_id.parse() else {
                continue;
            };
            let run_logger = spacebot::conversation::ProcessRunLogger::new(agent.db.sqlite.clone());
            let terminal = match run_logger.read_worker_terminal(worker_id).await {
                Ok(Some(terminal)) => terminal,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, %worker_id, "failed to read a worker terminal outcome");
                    continue;
                }
            };
            match global_task_store
                .finish_task_attempt(
                    &attempt.worker_id,
                    terminal.outcome_kind.into(),
                    Some(&terminal.result),
                )
                .await
            {
                Ok(true) => tracing::info!(
                    %worker_id,
                    outcome = terminal.outcome_kind.as_str(),
                    "recovered a committed outcome for an attempt left open"
                ),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, %worker_id, "failed to recover a task attempt outcome");
                }
            }
        }

        match global_task_store.reconcile_interrupted_attempts().await {
            Ok(closed) if closed > 0 => {
                tracing::info!(
                    attempts = closed,
                    "closed task attempts interrupted by an exit"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "failed to reconcile interrupted task attempts");
            }
        }
    }

    // Resume idle interactive workers that survived the restart.
    // For each idle worker, pre-create the channel if needed and spawn
    // the resumed worker into its state so follow-ups route correctly.
    if agents_initialized {
        for (agent_id, agent) in agents.iter() {
            let run_logger = spacebot::conversation::ProcessRunLogger::new(agent.db.sqlite.clone());
            let idle_workers = match run_logger
                .get_idle_interactive_workers(&agent.config.id)
                .await
            {
                Ok(workers) => workers,
                Err(error) => {
                    tracing::warn!(agent_id = %agent_id, %error, "failed to query idle workers");
                    continue;
                }
            };
            if idle_workers.is_empty() {
                continue;
            }
            tracing::info!(
                agent_id = %agent_id,
                idle_count = idle_workers.len(),
                "found idle interactive workers to resume"
            );

            // Group idle workers by channel_id
            let mut by_channel: HashMap<
                String,
                Vec<&spacebot::conversation::history::IdleWorkerRow>,
            > = HashMap::new();
            let mut detached_workers = Vec::new();
            for worker in &idle_workers {
                if let Some(channel_id) = &worker.channel_id {
                    by_channel
                        .entry(channel_id.clone())
                        .or_default()
                        .push(worker);
                } else {
                    detached_workers.push(worker);
                }
            }

            for (conversation_id, workers) in by_channel {
                if conversation_id == spacebot::agent::autonomy::AUTONOMY_CONVERSATION_ID {
                    let Some(supervisor) = agent.autonomy_supervisor.as_ref() else {
                        tracing::warn!(agent_id = %agent_id, "autonomy supervisor missing during idle worker recovery");
                        continue;
                    };
                    let state = supervisor.channel_state();
                    for idle_worker in workers {
                        if idle_worker.worker_type == "opencode"
                            && idle_worker.opencode_session_id.is_none()
                        {
                            if let Err(error) = run_logger.retire_idle_worker(&idle_worker.id).await
                            {
                                tracing::warn!(worker_id = %idle_worker.id, %error, "failed to retire idle autonomy worker");
                            }
                            continue;
                        }
                        let restoration_context =
                            spacebot::agent::channel_dispatch::WorkerRestorationContext {
                                deps: agent.deps.clone(),
                                channel_id: Some(state.channel_id.clone()),
                                process_run_logger: run_logger.clone(),
                                screenshot_dir: agent.config.screenshot_dir(),
                                logs_dir: agent.config.logs_dir(),
                                worker_context: state.worker_context_settings.read().await.clone(),
                                model_overrides: state.model_overrides.clone(),
                            };
                        match spacebot::agent::channel_dispatch::restore_idle_worker_into_registry(
                            &restoration_context,
                            idle_worker,
                        )
                        .await
                        {
                            Ok(worker_id) => tracing::info!(
                                %worker_id,
                                agent_id = %agent_id,
                                "resumed retained autonomy worker"
                            ),
                            Err(reason) => {
                                if let Err(error) =
                                    run_logger.retire_idle_worker(&idle_worker.id).await
                                {
                                    tracing::warn!(worker_id = %idle_worker.id, %error, "failed to retire autonomy worker after resume failure");
                                }
                                tracing::info!(worker_id = %idle_worker.id, %reason, "retired retained autonomy worker after resume failure");
                            }
                        }
                    }
                    continue;
                }

                let resolved_settings = load_worker_restoration_settings(
                    &agent.deps.sqlite_pool,
                    agent_id,
                    &conversation_id,
                )
                .await;
                let restoration_context =
                    spacebot::agent::channel_dispatch::WorkerRestorationContext {
                        deps: agent.deps.clone(),
                        channel_id: Some(Arc::from(conversation_id.as_str())),
                        process_run_logger: run_logger.clone(),
                        screenshot_dir: agent.config.screenshot_dir(),
                        logs_dir: agent.config.logs_dir(),
                        worker_context: resolved_settings.worker_context.clone(),
                        model_overrides: Arc::new(resolved_settings),
                    };
                for idle_worker in &workers {
                    match spacebot::agent::channel_dispatch::restore_idle_worker_into_registry(
                        &restoration_context,
                        idle_worker,
                    )
                    .await
                    {
                        Ok(worker_id) => tracing::info!(
                            %worker_id,
                            channel_id = %conversation_id,
                            "restored idle worker directly into agent registry"
                        ),
                        Err(reason) => {
                            if let Err(error) = run_logger.retire_idle_worker(&idle_worker.id).await
                            {
                                tracing::warn!(worker_id = %idle_worker.id, %error, "failed to retire idle worker");
                            }
                            tracing::info!(worker_id = %idle_worker.id, %reason, "retired idle worker after restoration failure");
                        }
                    }
                }
            }
            let detached_context = spacebot::agent::channel_dispatch::WorkerRestorationContext {
                deps: agent.deps.clone(),
                channel_id: None,
                process_run_logger: run_logger.clone(),
                screenshot_dir: agent.config.screenshot_dir(),
                logs_dir: agent.config.logs_dir(),
                worker_context: Default::default(),
                model_overrides: Arc::new(Default::default()),
            };
            for idle_worker in detached_workers {
                if let Err(reason) =
                    spacebot::agent::channel_dispatch::restore_idle_worker_into_registry(
                        &detached_context,
                        idle_worker,
                    )
                    .await
                {
                    if let Err(error) = run_logger.retire_idle_worker(&idle_worker.id).await {
                        tracing::warn!(worker_id = %idle_worker.id, %error, "failed to retire detached idle worker");
                    }
                    tracing::warn!(worker_id = %idle_worker.id, %reason, "failed to restore detached idle worker");
                }
            }
        }
    }

    if agents_initialized {
        for agent in agents.values() {
            agent.deps.autonomy_control.activate();
        }
    }

    // Announce a completed self-restart back to the channel that requested it.
    // Delivery goes through the proactive broadcast path (bounded retry with
    // backoff) rather than channel injection, which would sit queued until the
    // next inbound message on that exact channel. In setup mode the marker is
    // left in place for the fully-initialized boot that follows.
    if agents_initialized
        && let Some(pending) = spacebot::lifecycle::PendingRestart::take(&config.instance_dir)
    {
        let age = chrono::Utc::now().signed_duration_since(pending.requested_at);
        if age > chrono::Duration::minutes(15) {
            tracing::info!(reason = %pending.reason, "skipping stale restart announcement");
        } else if let (Some(adapter), Some(target)) = (pending.adapter, pending.target) {
            let manager = messaging_manager.clone();
            let note = format!("Back online after restart ({}).", pending.reason);
            tokio::spawn(async move {
                if let Err(error) = manager
                    .broadcast_proactive(&adapter, &target, spacebot::OutboundResponse::Text(note))
                    .await
                {
                    tracing::warn!(
                        %error,
                        adapter,
                        target,
                        "failed to deliver restart announcement"
                    );
                }
            });
        } else {
            tracing::info!(reason = %pending.reason, "restart complete, no channel to announce to");
        }
    }

    // Main event loop: route inbound messages to agent channels. The break
    // reason is recorded per arm: only a lifecycle-driven break may carry
    // Restart. An operator signal (SIGTERM, Ctrl-C) always means exit, even
    // when it races an armed restart whose channel value has already flipped.
    let mut final_state = spacebot::lifecycle::LifecycleState::Exit;
    loop {
        // Poll the inbound stream if it exists, otherwise yield a never-resolving future
        let inbound_next = async {
            match inbound_stream.as_mut() {
                Some(stream) => stream.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(mut message) = inbound_next, if agents_initialized => {
                let mut binding_settings: Option<spacebot::conversation::ConversationSettings> = None;
                let mut binding_authority: Option<Vec<String>> = None;
                let agent_id = if let Some(existing) = message.agent_id.as_ref() {
                    // Preassigned agent (portal sends set `agent_id` up
                    // front): the binding scan still runs so binding-level
                    // settings and authority apply to this scope.
                    let current_bindings = bindings.load();
                    if let Some(binding) = spacebot::config::matched_binding(&current_bindings, &message) {
                        binding_settings = binding.settings.clone();
                        binding_authority = binding.authority.clone();
                    }
                    existing.clone()
                } else {
                    let current_bindings = bindings.load();
                    let Some((resolved, matched_settings)) = spacebot::config::resolve_agent_for_message(
                        &current_bindings,
                        &message,
                        &default_agent_id,
                    ) else {
                        // Message suppressed by require_mention — drop it.
                        continue;
                    };
                    binding_settings = matched_settings;
                    binding_authority = spacebot::config::matched_binding_authority(&current_bindings, &message);
                    message.agent_id = Some(resolved.clone());
                    resolved
                };

                let conversation_id = message.conversation_id.clone();
                let channel_key = ActiveChannelKey::new(agent_id.to_string(), conversation_id.clone());

                // Slash-command dispatch, in the messaging layer before the
                // channel queue. Control commands execute on the control
                // plane without creating an inbound message, so they land
                // even while the channel is mid-turn; Agent commands are
                // rewritten to structured Command content and forwarded.
                if let Some(agent) = agents.get(&agent_id) {
                    let turn_active = {
                        let channel_ref: spacebot::ChannelId = Arc::from(conversation_id.as_str());
                        match agent.deps.process_control_registry.channel_handle(&channel_ref).await {
                            Some(handle) => handle.turn_active(),
                            None => false,
                        }
                    };
                    let authority_snapshot = authority_defaults.load();
                    let scope = spacebot::commands::dispatch::DispatchScope {
                        binding_authority: binding_authority.as_deref(),
                        adapter_defaults: &authority_snapshot,
                        binding_settings: binding_settings.as_ref(),
                        turn_active,
                    };
                    match spacebot::commands::dispatch::dispatch_inbound(
                        &mut message,
                        scope,
                        &agent.deps,
                        &messaging_manager,
                    )
                    .await
                    {
                        spacebot::commands::dispatch::Dispatch::Handled => continue,
                        spacebot::commands::dispatch::Dispatch::Forward
                        | spacebot::commands::dispatch::Dispatch::ForwardCommand => {}
                    }

                    // A paused agent starts no new work. Commands dispatch
                    // above this line, so /pause off and /status still land.
                    if let Some(reason) = agent.deps.pause_reason() {
                        tracing::debug!(
                            agent_id = %agent_id,
                            conversation_id = %conversation_id,
                            reason = %reason,
                            "dropping inbound message while paused"
                        );
                        continue;
                    }
                }

                // Find or create a channel for this conversation
                if !active_channels.contains_key(&channel_key) {
                    let Some(agent) = agents.get(&agent_id) else {
                        tracing::warn!(
                            agent_id = %agent_id,
                            conversation_id = %conversation_id,
                            "message routed to unknown agent, dropping"
                        );
                        continue;
                    };

                    // Create outbound response channel
                    let (response_tx, mut response_rx) = mpsc::channel::<spacebot::RoutedResponse>(32);

                    // Subscribe to the agent's event bus
                    let event_rx = agent.deps.event_tx.subscribe();

                    let channel_id: spacebot::ChannelId = Arc::from(conversation_id.as_str());


                    // Load per-conversation settings.
                    // Resolution: per-channel DB override > binding defaults > agent defaults > system defaults
                    let resolved_settings = if message.adapter.as_deref() == Some("portal") {
                        // Portal: load from portal_conversations table.
                        let store = spacebot::conversation::PortalConversationStore::new(
                            agent.deps.sqlite_pool.clone(),
                        );
                        match store.get(agent_id.as_ref(), &conversation_id).await {
                            Ok(Some(conv)) => {
                                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                                    conv.settings.as_ref(),
                                    binding_settings.as_ref(),
                                    None,
                                )
                            }
                            Ok(None) => {
                                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                                    None,
                                    binding_settings.as_ref(),
                                    None,
                                )
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    %conversation_id,
                                    "failed to load portal conversation settings, falling back to binding defaults"
                                );
                                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                                    None,
                                    binding_settings.as_ref(),
                                    None,
                                )
                            }
                        }
                    } else {
                        // Platform channels: load from channel_settings table.
                        let store = spacebot::conversation::ChannelSettingsStore::new(
                            agent.deps.sqlite_pool.clone(),
                        );
                        match store.get(agent_id.as_ref(), &conversation_id).await {
                            Ok(Some(settings)) => {
                                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                                    Some(&settings),
                                    binding_settings.as_ref(),
                                    None,
                                )
                            }
                            Ok(None) => {
                                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                                    None,
                                    binding_settings.as_ref(),
                                    None,
                                )
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    %conversation_id,
                                    "failed to load channel settings, falling back to binding defaults"
                                );
                                spacebot::conversation::settings::ResolvedConversationSettings::resolve(
                                    None,
                                    binding_settings.as_ref(),
                                    None,
                                )
                            }
                        }
                    };

                    let (mut channel, channel_tx) = spacebot::agent::channel::Channel::new(
                        channel_id,
                        spacebot::agent::channel::ChannelKind::User,
                        agent.deps.clone(),
                        response_tx,
                        event_rx,
                        agent.config.screenshot_dir(),
                        agent.config.logs_dir(),
                        Some(api_state.live_process_transcripts.clone()),
                        resolved_settings,
                        None, // no cron outcome for normal channels
                        None, // no autonomy run for normal channels
                    );
                    let channel_registration_id = agent
                        .deps
                        .process_control_registry
                        .register_channel(channel.id.clone(), channel.control_handle().downgrade())
                        .await;

                    // Register the channel's status block with the API for snapshot queries
                    api_state.register_channel_status(
                        agent_id.to_string(),
                        conversation_id.clone(),
                        channel.state.status_block.clone(),
                    ).await;

                    // Register the channel state for API-driven cancellation
                    api_state.register_channel_state(
                        conversation_id.clone(),
                        channel.state.clone(),
                    ).await;

                    // Backfill recent message history from the platform.
                    // The transcript is injected into the system prompt (not chat
                    // history) so the LLM treats it as read-only system context
                    // rather than actionable user messages.
                    let backfill_count = agent.config.history_backfill_count();
                    if backfill_count > 0 {
                        match messaging_manager.fetch_history(&message, backfill_count).await {
                            Ok(history_messages) => {
                                if let Some(transcript) =
                                    render_platform_history_backfill(&history_messages)
                                {
                                    channel.set_backfill_transcript(transcript);

                                    tracing::info!(
                                        conversation_id = %conversation_id,
                                        message_count = history_messages.len(),
                                        "backfilled channel history into system prompt"
                                    );
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "failed to backfill channel history");
                            }
                        }
                    }

                    // Spawn the channel's event loop
                    let cleanup_channel_id = conversation_id.clone();
                    let cleanup_agent_id = agent_id.to_string();
                    let process_control_registry = agent.deps.process_control_registry.clone();
                    let api_state_for_cleanup = api_state.clone();
                    tokio::spawn(async move {
                        if let Err(error) = channel.run().await {
                            tracing::error!(%error, "channel event loop failed");
                        }

                        let scoped_channel_id: spacebot::ChannelId =
                            Arc::from(cleanup_channel_id.as_str());
                        process_control_registry
                            .unregister_channel(&scoped_channel_id, channel_registration_id)
                            .await;
                        api_state_for_cleanup
                            .unregister_channel_status(&cleanup_agent_id, &cleanup_channel_id)
                            .await;
                        api_state_for_cleanup
                            .unregister_channel_state(&cleanup_channel_id)
                            .await;
                    });

                    // Spawn outbound response routing: reads from response_rx,
                    // sends to the messaging adapter and forwards to SSE
                    let messaging_for_outbound = messaging_manager.clone();
                    let outbound_conversation_id = conversation_id.clone();
                    let api_event_tx = api_state.event_tx.clone();
                    let sse_agent_id = agent_id.to_string();
                    let sse_channel_id = conversation_id.clone();
                    let outbound_handle = tokio::spawn(async move {
                        while let Some(routed) = response_rx.recv().await {
                            let spacebot::RoutedResponse { response, target, delivery_receipt } = routed;
                            forward_sse_event(&api_event_tx, &sse_agent_id, &sse_channel_id, &response);
                            let delivery = route_outbound(&messaging_for_outbound, &target, response).await;
                            if let Some(receipt) = delivery_receipt {
                                receipt.send(delivery).ok();
                            }
                        }
                        tracing::debug!(
                            conversation_id = %outbound_conversation_id,
                            "outbound response channel closed"
                        );
                    });

                    active_channels.insert(channel_key.clone(), ActiveChannel {
                        relay: spacebot::agent::inbound_relay::spawn(
                            &conversation_id,
                            &agent_id,
                            channel_tx,
                        ),
                        _outbound_handle: outbound_handle,
                    });

                    tracing::info!(
                        conversation_id = %conversation_id,
                        agent_id = %agent_id,
                        "new channel created"
                    );
                }

                // Forward the message to the channel
                if let Some(relay) = active_channels
                    .get(&channel_key)
                    .map(|active| active.relay.clone())
                {
                    let mut pending_delivery_failed = false;
                    if let Some(pending_injections) = deferred_injections.remove(&channel_key) {
                        let mut remaining_injections = Vec::new();
                        let mut pending_injections = pending_injections.into_iter();

                        while let Some(injection_message) = pending_injections.next() {
                            if let Err(error) = relay.send(injection_message) {
                                tracing::warn!(
                                    conversation_id = %conversation_id,
                                    agent_id = %agent_id,
                                    "failed to deliver deferred injected message to channel"
                                );
                                remaining_injections.push(*error.0);
                                remaining_injections.extend(pending_injections);
                                // Also re-queue the current inbound message so it isn't lost
                                remaining_injections.push(message.clone());
                                deferred_injections
                                    .entry(channel_key.clone())
                                    .or_default()
                                    .extend(remaining_injections);
                                active_channels.remove(&channel_key);
                                pending_delivery_failed = true;
                                break;
                            }
                        }
                    }

                    if pending_delivery_failed {
                        continue;
                    }

                    // Emit inbound message to SSE clients
                    let sender_name = message.formatted_author.clone().or_else(|| {
                        message
                            .metadata
                            .get("sender_display_name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                    let inbound_attachments: Vec<spacebot::agent::channel_attachments::SavedAttachmentMeta> =
                        message.metadata
                            .get("portal_attachment_metas")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default();
                    api_state.event_tx.send(spacebot::api::ApiEvent::InboundMessage {
                        agent_id: agent_id.to_string(),
                        channel_id: conversation_id.clone(),
                        sender_name,
                        sender_id: message.sender_id.clone(),
                        text: message.content.to_string(),
                        system: false,
                        attachments: inbound_attachments,
                    }).ok();

                    if let Err(error) = relay.send(message) {
                        tracing::error!(
                            conversation_id = %conversation_id,
                            %error,
                            "failed to forward message to channel"
                        );
                        active_channels.remove(&channel_key);
                    }
                }
            }
            Some(agent) = agent_rx.recv() => {
                tracing::info!(agent_id = %agent.id, "registering new agent in main loop");
                agents.insert(agent.id.clone(), agent);
            }
            Some(agent_id) = agent_remove_rx.recv() => {
                let key: spacebot::AgentId = Arc::from(agent_id.as_str());
                if let Some(mut agent) = agents.remove(&key) {
                    if let Some(supervisor) = agent.autonomy_supervisor.take() {
                        supervisor.shutdown(false).await;
                    }
                    agent.deps.mcp_manager.disconnect_all().await;
                    tracing::info!(agent_id = %agent_id, "removed agent from main loop");
                } else {
                    tracing::warn!(agent_id = %agent_id, "agent not found in main loop for removal");
                }
            }
            // Cross-agent message injection (e.g. delegated task completion retrigger).
            // Forwards the injected message to the target channel if it exists.
            Some(injection) = injection_rx.recv() => {
                let channel_key = ActiveChannelKey::new(
                    injection.agent_id.clone(),
                    injection.conversation_id.clone(),
                );

                if let Some(relay) = active_channels
                    .get(&channel_key)
                    .map(|active| active.relay.clone())
                {
                    if let Err(error) = relay.send(injection.message.clone()) {
                        tracing::warn!(
                            %error,
                            conversation_id = %injection.conversation_id,
                            agent_id = %injection.agent_id,
                            "failed to forward injected message to channel"
                        );
                        active_channels.remove(&channel_key);
                        queue_deferred_injection(&mut deferred_injections, injection);
                    } else {
                        tracing::info!(
                            conversation_id = %injection.conversation_id,
                            agent_id = %injection.agent_id,
                            "forwarded cross-agent injection to active channel"
                        );
                    }
                } else {
                    queue_deferred_injection(&mut deferred_injections, injection);
                    tracing::info!(
                        conversation_id = %channel_key.conversation_id,
                        agent_id = %channel_key.agent_id,
                        "injection target channel not active, notification deferred until that exact channel resumes"
                    );
                }
            }
            Some(_event) = provider_rx.recv(), if !agents_initialized => {
                tracing::info!("providers configured, initializing agents");

                // Reload config from disk to pick up new keys
                let new_config = if config_path.exists() {
                    spacebot::config::Config::load_from_path(&config_path)
                } else {
                    let instance_dir = config_path.parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    spacebot::config::Config::load_from_env(&instance_dir)
                };

                match new_config {
                    Ok(new_config) => {
                        api_state.set_agent_configs(configured_agent_infos(&new_config));

                        if has_provider_credentials(&new_config.llm, &new_config.instance_dir) {
                        // Refresh in-memory defaults so newly created agents
                        // inherit the latest routing from the updated config.
                        api_state.set_defaults_config(new_config.defaults.clone()).await;

                        // Rebuild LlmManager with the new keys
                        match spacebot::llm::LlmManager::with_instance_dir(
                            new_config.llm.clone(),
                            new_config.instance_dir.clone(),
                        )
                        .await
                        {
                            Ok(new_llm) => {
                                let new_llm_manager = Arc::new(new_llm);
                                // Ceilings live on the manager, so the
                                // replacement starts with none and every agent
                                // built after setup would send unbounded.
                                new_llm_manager.set_default_context_ceiling(
                                    new_config.defaults.context_window,
                                );
                                api_state.set_llm_manager(new_llm_manager.clone()).await;
                                // Update agent_humans from the reloaded config
                                // before initialize_agents so agents see the
                                // latest [[humans]] entries.
                                agent_humans.store(Arc::new(new_config.humans.clone()));
                                let mut new_watcher_agents = Vec::new();
                                let mut new_discord_permissions = None;
                                let mut new_slack_permissions = None;
                                let mut new_telegram_permissions = None;
                                let mut new_twitch_permissions = None;
                                let mut new_mattermost_permissions = None;
                                let mut new_signal_permissions = None;
                                match initialize_agents(
                                    &new_config,
                                    &new_llm_manager,
                                    &embedding_model,
                                    &prompt_engine,
                                    &api_state,
                                    &mut agents,
                                    &mut messaging_manager,
                                    &mut inbound_stream,
                                    &mut cron_schedulers_for_shutdown,
                                    &mut _ingestion_handles,
                                    &mut _cortex_handles,
                                    &mut new_watcher_agents,
                                    &mut new_discord_permissions,
                                    &mut new_slack_permissions,
                                    &mut new_telegram_permissions,
                                    &mut new_twitch_permissions,
                                    &mut new_mattermost_permissions,
                                    &mut new_signal_permissions,
                                    agent_links.clone(),
                                    agent_humans.clone(),
                                    injection_tx.clone(),
                                    global_task_store.clone(),
                                    global_goal_store.clone(),
                                    global_wiki_store.clone(),
                                    global_project_store.clone(),
                                    global_notification_store.clone(),
                                    &bootstrapped_store,
                                ).await {
                                    Ok(()) => {
                                        agents_initialized = true;
                                        // Restart file watcher with the new agent data
                                        let _old_watcher = _file_watcher.take();
                                        _file_watcher = Some(spacebot::config::spawn_file_watcher(
                                            config_path.clone(),
                                            new_config.instance_dir.clone(),
                                            new_watcher_agents,
                                            new_discord_permissions,
                                            new_slack_permissions,
                                            new_telegram_permissions,
                                            new_twitch_permissions,
                                            new_mattermost_permissions,
                                            new_signal_permissions,
                                            bindings.clone(),
                                            authority_defaults.clone(),
                                            Some(messaging_manager.clone()),
                                            new_llm_manager.clone(),
                                            agent_links.clone(),
                                            agent_humans.clone(),
                                        ));
                                        tracing::info!("agents initialized after provider setup");
                                    }
                                    Err(error) => {
                                        tracing::error!(%error, "failed to initialize agents after provider setup");
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "failed to create LLM manager with new keys");
                            }
                        }
                        } else {
                            tracing::warn!("config reloaded but still no providers configured");
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to reload config after provider setup");
                    }
                }
            }
            state = shutdown_rx.wait_for(|state| *state != spacebot::lifecycle::LifecycleState::Running) => {
                tracing::info!("lifecycle signal received via IPC/API");
                // A dropped sender can't happen while `lifecycle` is alive in
                // this scope; treat it as a plain exit if it somehow does.
                final_state = state
                    .map(|state| *state)
                    .unwrap_or(spacebot::lifecycle::LifecycleState::Exit);
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown signal received");
                break;
            }
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM received");
                break;
            }
        }
    }

    // Graceful shutdown
    for agent in agents.values() {
        agent.deps.process_control_registry.close_admission().await;
    }
    drop(active_channels);

    for agent in agents.values_mut() {
        if let Some(supervisor) = agent.autonomy_supervisor.take() {
            supervisor
                .shutdown(final_state == spacebot::lifecycle::LifecycleState::Restart)
                .await;
        }
    }

    for scheduler in &cron_schedulers_for_shutdown {
        scheduler.shutdown().await;
    }
    drop(cron_schedulers_for_shutdown);

    for agent in agents.values() {
        if final_state == spacebot::lifecycle::LifecycleState::Restart {
            agent.deps.process_control_registry.detach_workers().await;
        } else {
            agent
                .deps
                .process_control_registry
                .drain_workers("daemon shutting down", std::time::Duration::from_secs(2))
                .await;
        }
    }

    messaging_manager.shutdown().await;

    // Close shared browsers inline — their Drop-spawned cleanup task would
    // race the process exit (or never run before an exec) and orphan Chromium
    // on every restart. Shutdowns run concurrently, each bounded so a stuck
    // browser (or a tool call still holding the lock) can't stall teardown
    // past the restart confirmation window.
    futures::future::join_all(agents.iter().filter_map(|(agent_id, agent)| {
        let shared_browser = agent.deps.runtime_config.shared_browser.clone()?;
        let agent_id = agent_id.clone();
        Some(async move {
            let teardown = async { shared_browser.lock().await.shutdown().await };
            if tokio::time::timeout(std::time::Duration::from_secs(10), teardown)
                .await
                .is_err()
            {
                tracing::warn!(%agent_id, "shared browser teardown timed out after 10s");
            }
        })
    }))
    .await;

    for (agent_id, agent) in agents {
        tracing::info!(%agent_id, "shutting down agent");
        agent.deps.mcp_manager.disconnect_all().await;
        agent.db.close().await;
    }

    tracing::info!("spacebot stopped");

    // Flush buffered OTLP spans before the process exits. Without this the
    // batch exporter drops any spans recorded in the last export interval.
    if let Some(provider) = otel_provider
        && let Err(error) = provider.shutdown()
    {
        tracing::warn!(%error, "failed to flush OTel spans on shutdown");
    }

    spacebot::daemon::cleanup(&paths);

    if final_state == spacebot::lifecycle::LifecycleState::Restart {
        tracing::info!(exe = %restart_spec.exe.display(), "re-executing for restart");
        // exec() replaces the process image: every fd (locks, sockets, DB
        // handles) closes atomically, and startup re-runs from main() as if
        // launched fresh. Only returns on failure.
        let error = restart_spec.exec();
        // In background mode stderr is redirected away, so the log file is the
        // only place this failure can be diagnosed.
        tracing::error!(%error, exe = %restart_spec.exe.display(), "restart exec failed");
        eprintln!("restart exec failed: {error}");
        std::process::exit(1);
    }

    // A restart may have been superseded by this shutdown after the restart
    // tool already persisted its marker; drop it so the next boot doesn't
    // announce a restart that never happened.
    spacebot::lifecycle::PendingRestart::discard(&config.instance_dir);

    // Force exit — detached tasks (e.g. the serenity gateway client) may keep
    // the tokio runtime alive after all owned resources have been cleaned up.
    std::process::exit(0);
}

/// Initialize agents, messaging adapters, cron, cortex, and ingestion.
/// Extracted so it can be called either at startup or after providers are configured.
async fn wait_for_startup_warmup_tasks(
    startup_warmup: &mut tokio::task::JoinSet<()>,
    timeout: std::time::Duration,
) -> bool {
    let wait_all = async {
        while let Some(result) = startup_warmup.join_next().await {
            if let Err(error) = result {
                if error.is_cancelled() {
                    tracing::warn!(%error, "startup warmup task cancelled");
                } else {
                    tracing::error!(%error, "startup warmup task panicked");
                }
            }
        }
    };

    if tokio::time::timeout(timeout, wait_all).await.is_err() {
        startup_warmup.abort_all();
        true
    } else {
        false
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn initialize_agents(
    config: &spacebot::config::Config,
    llm_manager: &Arc<spacebot::llm::LlmManager>,
    embedding_model: &Arc<spacebot::memory::EmbeddingModel>,
    prompt_engine: &spacebot::prompts::PromptEngine,
    api_state: &Arc<spacebot::api::ApiState>,
    agents: &mut HashMap<spacebot::AgentId, spacebot::Agent>,
    messaging_manager: &mut Arc<spacebot::messaging::MessagingManager>,
    inbound_stream: &mut Option<
        std::pin::Pin<Box<dyn futures::Stream<Item = spacebot::InboundMessage> + Send>>,
    >,
    cron_schedulers_for_shutdown: &mut Vec<Arc<spacebot::cron::Scheduler>>,
    ingestion_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    cortex_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    watcher_agents: &mut Vec<(
        String,
        std::path::PathBuf,
        std::path::PathBuf,
        Arc<spacebot::config::RuntimeConfig>,
        Arc<spacebot::mcp::McpManager>,
    )>,
    discord_permissions: &mut Option<Arc<ArcSwap<spacebot::config::DiscordPermissions>>>,
    slack_permissions: &mut Option<Arc<ArcSwap<spacebot::config::SlackPermissions>>>,
    telegram_permissions: &mut Option<Arc<ArcSwap<spacebot::config::TelegramPermissions>>>,
    twitch_permissions: &mut Option<Arc<ArcSwap<spacebot::config::TwitchPermissions>>>,
    mattermost_permissions: &mut Option<Arc<ArcSwap<spacebot::config::MattermostPermissions>>>,
    signal_permissions: &mut Option<Arc<ArcSwap<spacebot::config::SignalPermissions>>>,
    agent_links: Arc<ArcSwap<Vec<spacebot::links::AgentLink>>>,
    agent_humans: Arc<ArcSwap<Vec<spacebot::config::HumanDef>>>,
    injection_tx: tokio::sync::mpsc::Sender<spacebot::ChannelInjection>,
    global_task_store: Arc<spacebot::tasks::TaskStore>,
    global_goal_store: Arc<spacebot::goals::GoalStore>,
    global_wiki_store: Arc<spacebot::wiki::WikiStore>,
    global_project_store: Arc<spacebot::projects::ProjectStore>,
    global_notification_store: Arc<spacebot::notifications::NotificationStore>,
    bootstrapped_store: &Option<Arc<spacebot::secrets::store::SecretsStore>>,
) -> anyhow::Result<()> {
    let resolved_agents = config.resolve_agents();

    // Build agent name map for inter-agent message routing
    let agent_name_map: Arc<std::collections::HashMap<String, String>> = Arc::new(
        resolved_agents
            .iter()
            .map(|a| {
                let name = a.display_name.clone().unwrap_or_else(|| a.id.clone());
                (a.id.clone(), name)
            })
            .collect(),
    );

    // Wake-dispatch infrastructure for autonomy channels. The registry lives
    // on `api_state` so runtime agent-create / agent-delete paths can keep it
    // in sync without going through main.
    let wake_registry = api_state.wake_registry.clone();
    let wake_tx = spacebot::agent::wake::spawn_wake_manager(wake_registry.clone());
    api_state.wake_tx.store(Arc::new(Some(wake_tx.clone())));

    for agent_config in &resolved_agents {
        tracing::info!(agent_id = %agent_config.id, "initializing agent");

        // Ensure agent directories exist
        std::fs::create_dir_all(&agent_config.workspace).with_context(|| {
            format!(
                "failed to create workspace: {}",
                agent_config.workspace.display()
            )
        })?;
        std::fs::create_dir_all(&agent_config.data_dir).with_context(|| {
            format!(
                "failed to create data dir: {}",
                agent_config.data_dir.display()
            )
        })?;
        std::fs::create_dir_all(&agent_config.archives_dir).with_context(|| {
            format!(
                "failed to create archives dir: {}",
                agent_config.archives_dir.display()
            )
        })?;
        std::fs::create_dir_all(agent_config.ingest_dir()).with_context(|| {
            format!(
                "failed to create ingest dir: {}",
                agent_config.ingest_dir().display()
            )
        })?;
        std::fs::create_dir_all(agent_config.logs_dir()).with_context(|| {
            format!(
                "failed to create logs dir: {}",
                agent_config.logs_dir().display()
            )
        })?;
        std::fs::create_dir_all(agent_config.saved_dir()).with_context(|| {
            format!(
                "failed to create saved dir: {}",
                agent_config.saved_dir().display()
            )
        })?;
        for dir in [
            agent_config.notes_dir(),
            agent_config.research_dir(),
            agent_config.workspace_archive_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create workspace dir: {}", dir.display()))?;
        }

        // Per-agent database connections
        let db = spacebot::db::Db::connect(&agent_config.data_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to connect databases for agent '{}'",
                    agent_config.id
                )
            })?;

        let run_logger = spacebot::conversation::ProcessRunLogger::new(db.sqlite.clone());
        let orphaned_workers = run_logger
            .reconcile_running_workers_for_agent(
                &agent_config.id,
                "Worker interrupted: Spacebot restarted before completion.",
            )
            .await
            .with_context(|| {
                format!(
                    "failed to reconcile stale running workers for agent '{}'",
                    agent_config.id
                )
            })?;
        if orphaned_workers > 0 {
            tracing::warn!(
                agent_id = %agent_config.id,
                orphaned_workers,
                "marked stale running workers as failed during startup"
            );
        }

        // Per-agent settings store (redb-backed)
        let settings_path = agent_config.data_dir.join("settings.redb");
        let settings_store = Arc::new(
            spacebot::settings::SettingsStore::new(&settings_path).with_context(|| {
                format!(
                    "failed to initialize settings store for agent '{}'",
                    agent_config.id
                )
            })?,
        );

        // Per-agent record of every outgoing LLM request. Payloads land under
        // the data directory; the index shares the agent database.
        let prompt_record_store = Arc::new(spacebot::llm::PromptRecordStore::new(
            &agent_config.data_dir,
            db.sqlite.clone(),
            settings_store.prompt_debug_capture(),
        ));

        // Per-agent memory system
        let memory_store =
            spacebot::memory::MemoryStore::with_agent_id(db.sqlite.clone(), &agent_config.id);
        let project_store = global_project_store.clone();
        let embedding_table = spacebot::memory::EmbeddingTable::open_or_create(&db.lance)
            .await
            .with_context(|| {
                format!("failed to init embeddings for agent '{}'", agent_config.id)
            })?;

        // Ensure FTS index exists for full-text search queries
        if let Err(error) = embedding_table.ensure_fts_index().await {
            tracing::warn!(%error, agent = %agent_config.id, "failed to create FTS index");
        }

        // Chronicle embeddings are optional: a table that cannot be opened
        // disables session search for this run instead of aborting startup.
        let chronicle_table =
            match spacebot::memory::ChronicleEmbeddingTable::open_or_create(&db.lance).await {
                Ok(table) => {
                    if let Err(error) = table.ensure_fts_index().await {
                        tracing::warn!(
                            %error,
                            agent = %agent_config.id,
                            "failed to create chronicle FTS index"
                        );
                    }
                    Some(table)
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        agent = %agent_config.id,
                        "failed to init chronicle embeddings; session search disabled this run"
                    );
                    None
                }
            };

        let mut memory_search = spacebot::memory::MemorySearch::new(
            memory_store,
            embedding_table,
            embedding_model.clone(),
        );
        if let Some(table) = chronicle_table {
            memory_search = memory_search.with_chronicle_table(table);
        }
        let memory_search = Arc::new(memory_search);

        // One-time backfill of level-0 checkpoint embeddings (1.7), off the
        // boot path — the table serves vector search while it fills.
        {
            let memory_search = Arc::clone(&memory_search);
            let pool = db.sqlite.clone();
            let agent = agent_config.id.clone();
            tokio::spawn(async move {
                if let Err(error) = memory_search.backfill_chronicle_embeddings(&pool).await {
                    tracing::warn!(%error, %agent, "chronicle embedding backfill failed");
                }
            });
        }

        // Seed anchor memories for configured org humans (3.1a). Idempotent —
        // humans with an existing anchor are left alone.
        spacebot::tools::memory_save::seed_org_human_anchors(&memory_search, &config.humans).await;

        // Working memory event log (temporal situational awareness).
        let working_memory_timezone = {
            let user_tz = agent_config.user_timezone.as_deref();
            let cron_tz = agent_config.cron_timezone.as_deref();
            user_tz
                .or(cron_tz)
                .and_then(|tz_name| tz_name.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(chrono_tz::Tz::UTC)
        };
        let working_memory =
            spacebot::memory::WorkingMemoryStore::new(db.sqlite.clone(), working_memory_timezone);

        // Per-agent control and memory event buses (broadcast fan-out).
        let process_event_buses = spacebot::create_process_event_buses();
        let event_tx = process_event_buses.control;
        let memory_event_tx = process_event_buses.memory;
        let tool_output_tx = process_event_buses.tool_output;

        let agent_id: spacebot::AgentId = Arc::from(agent_config.id.as_str());
        let mcp_manager = Arc::new(spacebot::mcp::McpManager::new(agent_config.mcp.clone()));
        mcp_manager.connect_all().await;

        // Scaffold identity templates if missing, then load.
        // Identity files live in the agent root (identity_dir), outside the
        // workspace sandbox boundary.
        spacebot::identity::scaffold_identity_files(&agent_config.identity_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to scaffold identity files for agent '{}'",
                    agent_config.id
                )
            })?;
        let identity = spacebot::identity::Identity::load(&agent_config.identity_dir).await;

        // Load skills (instance-level, then workspace overrides)
        let skills =
            spacebot::skills::SkillSet::load(&config.skills_dir(), &agent_config.skills_dir())
                .await;

        // Build the RuntimeConfig with all hot-reloadable values
        let runtime_config = Arc::new(spacebot::config::RuntimeConfig::new(
            &config.instance_dir,
            agent_config,
            &config.defaults,
            prompt_engine.clone(),
            identity,
            skills,
        ));

        runtime_config.set_settings(settings_store.clone());
        let skill_usage_store = Arc::new(spacebot::skills::SkillUsageStore::new(db.sqlite.clone()));
        runtime_config.set_skill_usage(skill_usage_store.clone());
        {
            let skill_names: Vec<String> = runtime_config
                .skills
                .load()
                .iter()
                .map(|s| s.name.to_lowercase())
                .collect();
            if let Err(error) = skill_usage_store.seed(&skill_names).await {
                tracing::warn!(%error, agent = %agent_config.id, "failed to seed skill usage rows");
            }
        }
        runtime_config
            .prompt_records
            .store(Arc::new(Some(prompt_record_store.clone())));
        if let Err(error) = settings_store.set_worker_log_mode(config.defaults.worker_log_mode) {
            tracing::warn!(%error, agent = %agent_config.id, "failed to set worker_log_mode from config");
        }
        // Config seeds the home channel; a value set at runtime owns it from
        // then on and is never clobbered by a reload.
        if let Some(home) = config.defaults.home_channel.as_deref() {
            match settings_store.adopt_home_channel(home) {
                Ok(true) => {
                    tracing::info!(agent = %agent_config.id, home_channel = %home, "seeded home channel from config")
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, agent = %agent_config.id, "failed to seed home_channel from config")
                }
            }
        }

        // Share the instance-level secrets store with this agent.
        if let Some(secrets_store) = bootstrapped_store {
            runtime_config.set_secrets(secrets_store.clone());
            spacebot::config::set_resolve_secrets_store(secrets_store.clone());
        }

        watcher_agents.push((
            agent_config.id.clone(),
            agent_config.workspace.clone(),
            agent_config.identity_dir.clone(),
            runtime_config.clone(),
            mcp_manager.clone(),
        ));

        let sandbox = std::sync::Arc::new(
            spacebot::sandbox::Sandbox::new(
                runtime_config.sandbox.clone(),
                agent_config.workspace.clone(),
                &config.instance_dir,
                agent_config.data_dir.clone(),
                agent_id.clone(),
            )
            .await,
        );

        // Wire the instance-level secrets store into the sandbox for tool secret injection.
        if let Some(secrets_store) = &bootstrapped_store {
            sandbox.set_secrets_store(secrets_store.clone());
        }

        // Inject active project root paths into the sandbox allowlist so
        // workers can access project directories even outside the workspace.
        spacebot::projects::refresh_sandbox_project_paths(&project_store, &sandbox).await;

        let deps = spacebot::AgentDeps {
            agent_id: agent_id.clone(),
            memory_search,
            llm_manager: llm_manager.clone(),
            mcp_manager,
            task_store: global_task_store.clone(),
            goal_store: global_goal_store.clone(),
            wake_event_store: Arc::new(spacebot::wakes::WakeEventStore::new(db.sqlite.clone())),
            autonomy_ceiling: api_state.autonomy_ceiling.clone(),
            wake_def_store: Arc::new(spacebot::wakes::WakeDefStore::new(db.sqlite.clone())),
            autonomy_run_store: Arc::new(spacebot::wakes::AutonomyRunStore::new(db.sqlite.clone())),
            autonomy_control: spacebot::agent::autonomy::AutonomyControl::default(),
            project_store: project_store.clone(),
            cron_tool: None,
            runtime_config,
            event_tx,
            memory_event_tx,
            tool_output_tx,
            sqlite_pool: db.sqlite.clone(),
            messaging_manager: None,
            sandbox,
            links: agent_links.clone(),
            agent_names: agent_name_map.clone(),
            humans: agent_humans.clone(),
            process_control_registry: Arc::new(
                spacebot::agent::process_control::ProcessControlRegistry::new(),
            ),
            injection_tx: injection_tx.clone(),
            working_memory,
            api_state: Some(api_state.clone()),
            wiki_store: Some(global_wiki_store.clone()),
            wake_tx: Some(wake_tx.clone()),
        };

        let agent = spacebot::Agent {
            id: agent_id.clone(),
            config: agent_config.clone(),
            db,
            deps,
            autonomy_supervisor: None,
        };

        // Register with the wake manager so external triggers can reach this agent.
        wake_registry
            .write()
            .await
            .insert(agent_id.clone(), agent.deps.clone());

        tracing::info!(agent_id = %agent_config.id, "agent initialized");
        agents.insert(agent_id, agent);
    }

    // Pre-register both sides of every link channel so they appear in each
    // agent's channel list from boot. The actual Channel instances are spawned
    // on-demand when the first message arrives; this just creates the DB records
    // so the UI can display them.
    {
        let all_links = agent_links.load();
        let empty_meta = std::collections::HashMap::new();
        for link in all_links.iter() {
            let from_channel = link.channel_id_for(&link.from_agent_id);
            let to_channel = link.channel_id_for(&link.to_agent_id);

            if let Some(agent) = agents.get(&Arc::from(link.from_agent_id.as_str())) {
                let store = spacebot::conversation::ChannelStore::new(agent.db.sqlite.clone());
                store.upsert(&from_channel, &empty_meta);
            }
            if let Some(agent) = agents.get(&Arc::from(link.to_agent_id.as_str())) {
                let store = spacebot::conversation::ChannelStore::new(agent.db.sqlite.clone());
                store.upsert(&to_channel, &empty_meta);
            }
        }
        if !all_links.is_empty() {
            tracing::info!(link_count = all_links.len(), "pre-registered link channels");
        }
    }

    tracing::info!(agent_count = agents.len(), "all agents initialized");

    // Record startup in each agent's working memory.
    for agent in agents.values() {
        agent
            .deps
            .working_memory
            .emit(
                spacebot::memory::WorkingMemoryEventType::System,
                format!("Agent started ({})", agent.config.id),
            )
            .importance(0.3)
            .record();
    }

    // Wire agent event streams, DB pools, and config summaries into the API server
    {
        let mut agent_pools = std::collections::HashMap::new();
        let mut process_control_registries = std::collections::HashMap::new();
        let mut agent_configs = Vec::new();
        let mut memory_searches = std::collections::HashMap::new();
        let mut mcp_managers = std::collections::HashMap::new();
        let mut agent_workspaces = std::collections::HashMap::new();
        let mut agent_identity_dirs = std::collections::HashMap::new();
        let mut agent_data_dirs = std::collections::HashMap::new();
        let mut runtime_configs = std::collections::HashMap::new();
        let mut sandboxes = std::collections::HashMap::new();
        for (agent_id, agent) in agents.iter() {
            let event_rx = agent.deps.event_tx.subscribe();
            api_state.register_agent_events(agent_id.to_string(), event_rx);
            let tool_output_rx = agent.deps.tool_output_tx.subscribe();
            api_state.register_tool_output_stream(agent_id.to_string(), tool_output_rx);
            agent_pools.insert(agent_id.to_string(), agent.db.sqlite.clone());
            process_control_registries.insert(
                agent_id.to_string(),
                agent.deps.process_control_registry.clone(),
            );
            memory_searches.insert(agent_id.to_string(), agent.deps.memory_search.clone());
            mcp_managers.insert(agent_id.to_string(), agent.deps.mcp_manager.clone());
            agent_workspaces.insert(agent_id.to_string(), agent.config.workspace.clone());
            agent_identity_dirs.insert(agent_id.to_string(), agent.config.identity_dir.clone());
            agent_data_dirs.insert(agent_id.to_string(), agent.config.data_dir.clone());
            runtime_configs.insert(agent_id.to_string(), agent.deps.runtime_config.clone());
            sandboxes.insert(agent_id.to_string(), agent.deps.sandbox.clone());
            agent_configs.push(spacebot::api::AgentInfo {
                id: agent.config.id.clone(),
                display_name: agent.config.display_name.clone(),
                role: agent.config.role.clone(),
                gradient_start: agent.config.gradient_start.clone(),
                gradient_end: agent.config.gradient_end.clone(),
                workspace: agent.config.workspace.to_string_lossy().to_string(),
                context_window: agent.config.context_window,
                max_turns: agent.config.max_turns,
                max_concurrent_branches: agent.config.max_concurrent_branches,
                max_concurrent_workers: agent.config.max_concurrent_workers,
            });
        }
        api_state.set_agent_pools(agent_pools);
        api_state.set_process_control_registries(process_control_registries);
        api_state.set_agent_configs(agent_configs);
        api_state.set_memory_searches(memory_searches);
        api_state.set_mcp_managers(mcp_managers);
        api_state.set_project_store(global_project_store.clone());
        api_state.set_runtime_configs(runtime_configs);
        api_state.set_agent_workspaces(agent_workspaces);
        api_state.set_agent_identity_dirs(agent_identity_dirs);
        api_state.set_agent_data_dirs(agent_data_dirs);
        api_state.set_sandboxes(sandboxes);
        // Wire the instance-level secrets store into the API state.
        if let Some(store) = &bootstrapped_store {
            api_state.set_secrets_store(store.clone());
        }
        api_state.set_instance_dir(config.instance_dir.clone());
    }

    // Run a startup warmup pass for every agent before adapters begin receiving
    // inbound traffic. This reduces first-message cold-start latency.
    {
        const STARTUP_WARMUP_WAIT_SECS: u64 = 30;
        let mut startup_warmup = tokio::task::JoinSet::new();

        for (agent_id, agent) in agents.iter() {
            // Dormant agents stay cold at startup — running warmup here would
            // touch model/embedding load paths the dormant-mode contract
            // promises to skip until an explicit wake.
            if agent.deps.runtime_config.cortex.load().mode.is_dormant() {
                tracing::info!(agent_id = %agent_id, "startup warmup skipped: dormant");
                continue;
            }
            let deps = agent.deps.clone();
            let sqlite_pool = agent.db.sqlite.clone();
            let agent_id = agent_id.clone();
            startup_warmup.spawn(async move {
                let logger = spacebot::agent::cortex::CortexLogger::new(sqlite_pool);
                spacebot::agent::cortex::run_warmup_once(
                    &deps,
                    &logger,
                    "startup_pre_adapter",
                    false,
                )
                .await;
                let status = deps.runtime_config.warmup_status.load().as_ref().clone();
                tracing::info!(
                    agent_id = %agent_id,
                    state = ?status.state,
                    embedding_ready = status.embedding_ready,
                    refresh_age_secs = ?status.refresh_age_secs,
                    last_error = ?status.last_error,
                    "startup warmup pass finished"
                );
            });
        }

        if wait_for_startup_warmup_tasks(
            &mut startup_warmup,
            std::time::Duration::from_secs(STARTUP_WARMUP_WAIT_SECS),
        )
        .await
        {
            tracing::warn!(
                timeout_secs = STARTUP_WARMUP_WAIT_SECS,
                "startup warmup wait timed out; cancelled unfinished startup warmup tasks and continuing startup"
            );
        }
    }

    // Initialize messaging adapters
    let new_messaging_manager = spacebot::messaging::MessagingManager::new();

    // Shared Discord permissions (hot-reloadable via file watcher)
    *discord_permissions = config.messaging.discord.as_ref().map(|discord_config| {
        let perms =
            spacebot::config::DiscordPermissions::from_config(discord_config, &config.bindings);
        Arc::new(ArcSwap::from_pointee(perms))
    });
    if let Some(perms) = &*discord_permissions {
        api_state.set_discord_permissions(perms.clone()).await;
    }

    if let Some(discord_config) = &config.messaging.discord
        && discord_config.enabled
    {
        if !discord_config.token.is_empty() {
            let adapter = spacebot::messaging::discord::DiscordAdapter::new(
                "discord",
                &discord_config.token,
                discord_permissions.clone().ok_or_else(|| {
                    anyhow::anyhow!("discord permissions not initialized when discord is enabled")
                })?,
            );
            new_messaging_manager.register(adapter).await;
        }

        for instance in discord_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.token.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled discord instance with empty token");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "discord",
                Some(instance.name.as_str()),
            );
            let perms = Arc::new(ArcSwap::from_pointee(
                spacebot::config::DiscordPermissions::from_instance_config(
                    instance,
                    &config.bindings,
                ),
            ));
            let adapter = spacebot::messaging::discord::DiscordAdapter::new(
                runtime_key,
                &instance.token,
                perms,
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    // Shared Slack permissions (hot-reloadable via file watcher)
    *slack_permissions = config.messaging.slack.as_ref().map(|slack_config| {
        let perms = spacebot::config::SlackPermissions::from_config(slack_config, &config.bindings);
        Arc::new(ArcSwap::from_pointee(perms))
    });
    if let Some(perms) = &*slack_permissions {
        api_state.set_slack_permissions(perms.clone()).await;
    }

    if let Some(slack_config) = &config.messaging.slack
        && slack_config.enabled
    {
        if !slack_config.bot_token.is_empty() && !slack_config.app_token.is_empty() {
            match spacebot::messaging::slack::SlackAdapter::new(
                "slack",
                &slack_config.bot_token,
                &slack_config.app_token,
                slack_permissions.clone().ok_or_else(|| {
                    anyhow::anyhow!("slack permissions not initialized when slack is enabled")
                })?,
            ) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, "failed to build slack adapter");
                }
            }
        }

        for instance in slack_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.bot_token.is_empty() || instance.app_token.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled slack instance with missing tokens");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "slack",
                Some(instance.name.as_str()),
            );
            let perms = Arc::new(ArcSwap::from_pointee(
                spacebot::config::SlackPermissions::from_instance_config(
                    instance,
                    &config.bindings,
                ),
            ));
            match spacebot::messaging::slack::SlackAdapter::new(
                runtime_key,
                &instance.bot_token,
                &instance.app_token,
                perms,
            ) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, adapter = %instance.name, "failed to build named slack adapter");
                }
            }
        }
    }

    // Shared Telegram permissions (hot-reloadable via file watcher)
    *telegram_permissions = config.messaging.telegram.as_ref().map(|telegram_config| {
        let perms =
            spacebot::config::TelegramPermissions::from_config(telegram_config, &config.bindings);
        Arc::new(ArcSwap::from_pointee(perms))
    });

    if let Some(telegram_config) = &config.messaging.telegram
        && telegram_config.enabled
    {
        if !telegram_config.token.is_empty() {
            let adapter = spacebot::messaging::telegram::TelegramAdapter::new(
                "telegram",
                &telegram_config.token,
                telegram_permissions.clone().ok_or_else(|| {
                    anyhow::anyhow!("telegram permissions not initialized when telegram is enabled")
                })?,
            );
            new_messaging_manager.register(adapter).await;
        }

        for instance in telegram_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.token.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled telegram instance with empty token");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "telegram",
                Some(instance.name.as_str()),
            );
            let perms = Arc::new(ArcSwap::from_pointee(
                spacebot::config::TelegramPermissions::from_instance_config(
                    instance,
                    &config.bindings,
                ),
            ));
            let adapter = spacebot::messaging::telegram::TelegramAdapter::new(
                runtime_key,
                &instance.token,
                perms,
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    if let Some(email_config) = &config.messaging.email
        && email_config.enabled
    {
        if !email_config.imap_host.is_empty() {
            match spacebot::messaging::email::EmailAdapter::from_config(email_config) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, "failed to build email adapter");
                }
            }
        }

        for instance in email_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.imap_host.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled email instance with empty credentials");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "email",
                Some(instance.name.as_str()),
            );
            match spacebot::messaging::email::EmailAdapter::from_instance_config(
                runtime_key,
                instance,
            ) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, adapter = %instance.name, "failed to build named email adapter");
                }
            }
        }
    }

    if let Some(webhook_config) = &config.messaging.webhook
        && webhook_config.enabled
    {
        let adapter = spacebot::messaging::webhook::WebhookAdapter::new(
            webhook_config.port,
            &webhook_config.bind,
            webhook_config.auth_token.clone(),
        );
        new_messaging_manager.register(adapter).await;
    }

    // Shared Twitch permissions (hot-reloadable via file watcher)
    *twitch_permissions = config.messaging.twitch.as_ref().map(|twitch_config| {
        let perms =
            spacebot::config::TwitchPermissions::from_config(twitch_config, &config.bindings);
        Arc::new(ArcSwap::from_pointee(perms))
    });

    if let Some(twitch_config) = &config.messaging.twitch
        && twitch_config.enabled
    {
        let twitch_token_path = config.instance_dir.join("twitch_token.json");
        if !twitch_config.username.is_empty() && !twitch_config.oauth_token.is_empty() {
            let adapter = spacebot::messaging::twitch::TwitchAdapter::new(
                "twitch",
                &twitch_config.username,
                &twitch_config.oauth_token,
                twitch_config.client_id.clone(),
                twitch_config.client_secret.clone(),
                twitch_config.refresh_token.clone(),
                Some(twitch_token_path),
                twitch_config.channels.clone(),
                twitch_config.trigger_prefix.clone(),
                twitch_permissions.clone().ok_or_else(|| {
                    anyhow::anyhow!("twitch permissions not initialized when twitch is enabled")
                })?,
            );
            new_messaging_manager.register(adapter).await;
        }

        for instance in twitch_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.username.is_empty() || instance.oauth_token.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled twitch instance with missing credentials");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "twitch",
                Some(instance.name.as_str()),
            );
            let token_file_name = spacebot::config::named_twitch_token_file_name(&instance.name);
            let token_path = config.instance_dir.join(token_file_name);
            let perms = Arc::new(ArcSwap::from_pointee(
                spacebot::config::TwitchPermissions::from_instance_config(
                    instance,
                    &config.bindings,
                ),
            ));
            let adapter = spacebot::messaging::twitch::TwitchAdapter::new(
                runtime_key,
                &instance.username,
                &instance.oauth_token,
                instance.client_id.clone(),
                instance.client_secret.clone(),
                instance.refresh_token.clone(),
                Some(token_path),
                instance.channels.clone(),
                instance.trigger_prefix.clone(),
                perms,
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    // Shared Mattermost permissions (hot-reloadable via file watcher)
    *mattermost_permissions = config
        .messaging
        .mattermost
        .as_ref()
        .map(|mattermost_config| {
            let perms = spacebot::config::MattermostPermissions::from_config(
                mattermost_config,
                &config.bindings,
            );
            Arc::new(ArcSwap::from_pointee(perms))
        });

    if let Some(mattermost_config) = &config.messaging.mattermost
        && mattermost_config.enabled
    {
        if !mattermost_config.base_url.is_empty() && !mattermost_config.token.is_empty() {
            match spacebot::messaging::mattermost::MattermostAdapter::new(
                "mattermost",
                &mattermost_config.base_url,
                mattermost_config.token.as_str(),
                mattermost_config.team_id.as_deref().map(Arc::from),
                mattermost_config.max_attachment_bytes,
                mattermost_permissions.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "mattermost permissions not initialized when mattermost is enabled"
                    )
                })?,
            ) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, "failed to create mattermost adapter");
                }
            }
        }

        for instance in mattermost_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.base_url.is_empty() || instance.token.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled mattermost instance with missing credentials");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "mattermost",
                Some(instance.name.as_str()),
            );
            let perms = Arc::new(ArcSwap::from_pointee(
                spacebot::config::MattermostPermissions::from_instance_config(
                    instance,
                    &config.bindings,
                ),
            ));
            match spacebot::messaging::mattermost::MattermostAdapter::new(
                runtime_key,
                &instance.base_url,
                instance.token.as_str(),
                instance.team_id.as_deref().map(Arc::from),
                instance.max_attachment_bytes,
                perms,
            ) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, adapter = %instance.name, "failed to create named mattermost adapter");
                }
            }
        }
    }

    // Shared Signal permissions (hot-reloadable via file watcher)
    *signal_permissions = config.messaging.signal.as_ref().map(|signal_config| {
        let perms = spacebot::config::SignalPermissions::from_config(signal_config);
        Arc::new(ArcSwap::from_pointee(perms))
    });
    if let Some(perms) = &*signal_permissions {
        api_state.set_signal_permissions(perms.clone()).await;
    }

    // Signal: start default adapter (requires root enabled) and named instances (independent).
    // Unlike Discord/Telegram where named instances inherit the root enabled gate,
    // Signal named instances start independently when they have valid credentials
    // and their own enabled flag is set. This allows running multiple Signal accounts
    // without needing a "default" account enabled.
    let tmp_dir = config.instance_dir.join("tmp");
    if let Some(signal_config) = &config.messaging.signal {
        // Start default adapter only if root is enabled AND has credentials
        if signal_config.enabled
            && !signal_config.http_url.is_empty()
            && !signal_config.account.is_empty()
        {
            let adapter = spacebot::messaging::signal::SignalAdapter::new(
                "signal",
                &signal_config.http_url,
                &signal_config.account,
                signal_config.ignore_stories,
                signal_permissions.clone().ok_or_else(|| {
                    anyhow::anyhow!("signal permissions not initialized when signal is enabled")
                })?,
                tmp_dir.clone(),
            );
            new_messaging_manager.register(adapter).await;
        }

        // Start named instances regardless of root enabled flag (as long as config exists)
        for instance in signal_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.http_url.is_empty() || instance.account.is_empty() {
                tracing::warn!(adapter = %instance.name, "skipping enabled signal instance with missing credentials");
                continue;
            }
            let runtime_key = spacebot::config::binding_runtime_adapter_key(
                "signal",
                Some(instance.name.as_str()),
            );
            let perms = Arc::new(ArcSwap::from_pointee(
                spacebot::config::SignalPermissions::from_instance_config(instance),
            ));
            let adapter = spacebot::messaging::signal::SignalAdapter::new(
                runtime_key,
                &instance.http_url,
                &instance.account,
                instance.ignore_stories,
                perms,
                tmp_dir.clone(),
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    new_messaging_manager
        .seed_configured_fingerprints_from_registered()
        .await;

    let portal_agent_pools = agents
        .iter()
        .map(|(agent_id, agent)| (agent_id.to_string(), agent.db.sqlite.clone()))
        .collect();
    let portal_adapter = Arc::new(spacebot::messaging::portal::PortalAdapter::new(
        portal_agent_pools,
    ));
    portal_adapter.set_event_tx(api_state.event_tx.clone());
    new_messaging_manager
        .register_shared(portal_adapter.clone())
        .await;
    api_state.set_portal_adapter(portal_adapter);

    *messaging_manager = Arc::new(new_messaging_manager);
    api_state
        .set_messaging_manager(messaging_manager.clone())
        .await;

    // Start all messaging adapters and get the merged inbound stream
    let new_inbound = messaging_manager
        .start()
        .await
        .context("failed to start messaging adapters")?;
    *inbound_stream = Some(new_inbound);

    tracing::info!("messaging adapters started");

    // Initialize cron schedulers for each agent
    let mut cron_stores_map = std::collections::HashMap::new();
    let mut cron_schedulers_map = std::collections::HashMap::new();

    for (agent_id, agent) in agents.iter_mut() {
        let store = Arc::new(spacebot::cron::CronStore::new(agent.db.sqlite.clone()));
        agent.deps.messaging_manager = Some(messaging_manager.clone());

        // Seed built-in wakes, then reconcile config-owned wake definitions.
        // Builtins go first so a config id colliding with one is detected.
        if let Err(error) = spacebot::wakes::seed_builtin_wakes(&agent.deps.wake_def_store).await {
            tracing::warn!(agent_id = %agent_id, %error, "failed to seed builtin wakes");
        }
        if let Err(error) =
            spacebot::wakes::reconcile_config_wakes(&agent.deps.wake_def_store, &agent.config.wakes)
                .await
        {
            tracing::warn!(agent_id = %agent_id, %error, "failed to reconcile config wakes");
        }

        // Seed cron jobs from config into the database
        for cron_def in &agent.config.cron {
            let cron_config = spacebot::cron::CronConfig {
                id: cron_def.id.clone(),
                prompt: cron_def.prompt.clone(),
                cron_expr: cron_def.cron_expr.clone(),
                interval_secs: cron_def.interval_secs,
                delivery_target: cron_def.delivery_target.clone(),
                active_hours: cron_def.active_hours,
                enabled: cron_def.enabled,
                run_once: cron_def.run_once,
                next_run_at: None,
                timeout_secs: cron_def.timeout_secs,
            };
            if let Err(error) = store.save(&cron_config).await {
                tracing::warn!(
                    agent_id = %agent_id,
                    cron_id = %cron_def.id,
                    %error,
                    "failed to seed cron config"
                );
            }
        }

        // Load all enabled cron jobs and start the scheduler
        let cron_context = spacebot::cron::CronContext {
            deps: agent.deps.clone(),
            screenshot_dir: agent.config.screenshot_dir(),
            logs_dir: agent.config.logs_dir(),
            messaging_manager: messaging_manager.clone(),
            store: store.clone(),
        };

        let scheduler = Arc::new(spacebot::cron::Scheduler::new(cron_context));

        // Make cron store and scheduler available via RuntimeConfig
        agent
            .deps
            .runtime_config
            .set_cron(store.clone(), scheduler.clone());

        match store.load_all().await {
            Ok(configs) => {
                // Load last execution times so interval-based jobs can anchor
                // their first tick to the previous run, surviving restarts.
                let last_times = match store.last_execution_times().await {
                    Ok(times) => times,
                    Err(error) => {
                        tracing::warn!(agent_id = %agent_id, %error, "failed to load cron last execution times");
                        std::collections::HashMap::new()
                    }
                };
                for cron_config in configs {
                    let anchor = last_times.get(&cron_config.id).map(String::as_str);
                    if let Err(error) = scheduler.register_with_anchor(cron_config, anchor).await {
                        tracing::warn!(agent_id = %agent_id, %error, "failed to register cron job");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(agent_id = %agent_id, %error, "failed to load cron jobs from database");
            }
        }

        // Store cron tool on deps so each channel can register it on its own tool server
        let cron_tool = spacebot::tools::CronTool::new(
            store.clone(),
            scheduler.clone(),
            messaging_manager.clone(),
        );
        agent.deps.cron_tool = Some(cron_tool);

        cron_stores_map.insert(agent_id.to_string(), store);
        cron_schedulers_map.insert(agent_id.to_string(), scheduler.clone());
        cron_schedulers_for_shutdown.push(scheduler);
        tracing::info!(agent_id = %agent_id, "cron scheduler started");
    }

    // Set cron stores and schedulers on the API state
    api_state.set_cron_stores(cron_stores_map);
    api_state.set_cron_schedulers(cron_schedulers_map);
    tracing::info!("cron stores and schedulers registered with API state");

    for (agent_id, agent) in agents.iter_mut() {
        let supervisor = spacebot::agent::autonomy::spawn_autonomy_supervisor(agent.deps.clone());
        agent.autonomy_supervisor = Some(supervisor);
        tracing::info!(%agent_id, "resident autonomy channel started");
    }

    // Start memory ingestion loops for each agent
    for (agent_id, agent) in agents.iter() {
        let ingestion_config = **agent.deps.runtime_config.ingestion.load();
        if ingestion_config.enabled {
            let handle = spacebot::agent::ingestion::spawn_ingestion_loop(
                agent.config.ingest_dir(),
                agent.deps.clone(),
            );
            ingestion_handles.push(handle);
            tracing::info!(agent_id = %agent_id, "memory ingestion loop started");
        }
    }

    // Start cortex warmup, runtime, and association loops for each agent.
    // Skip every loop when the agent is in Dormant mode — those agents wake
    // only on external triggers (cross-agent message, cron fire, admin API).
    for (agent_id, agent) in agents.iter() {
        let cortex_mode = agent.deps.runtime_config.cortex.load().mode;
        if cortex_mode.is_dormant() {
            tracing::info!(
                agent_id = %agent_id,
                "cortex loops skipped: agent is in dormant mode"
            );
            continue;
        }

        let cortex_logger = spacebot::agent::cortex::CortexLogger::new(agent.db.sqlite.clone())
            .with_notifications(global_notification_store.clone(), agent_id.to_string());
        let warmup_handle =
            spacebot::agent::cortex::spawn_warmup_loop(agent.deps.clone(), cortex_logger.clone());
        cortex_handles.push(warmup_handle);
        tracing::info!(agent_id = %agent_id, "warmup loop started");

        let cortex_handle =
            spacebot::agent::cortex::spawn_cortex_loop(agent.deps.clone(), cortex_logger.clone());
        cortex_handles.push(cortex_handle);
        tracing::info!(agent_id = %agent_id, "cortex loop started");

        let association_handle =
            spacebot::agent::cortex::spawn_association_loop(agent.deps.clone(), cortex_logger);
        cortex_handles.push(association_handle);
        tracing::info!(agent_id = %agent_id, "cortex association loop started");
    }

    cortex_handles.push(spacebot::agent::maintenance::spawn_prompt_record_sweeper(
        wake_registry.clone(),
    ));

    // Spawn the instance-wide memory janitor when configured. Required for
    // dormant-mode agents (their cortex loop never runs maintenance);
    // additive on active-mode agents (idempotent).
    if config.memory_janitor.enabled {
        let janitor_handle = spacebot::agent::maintenance::spawn_memory_janitor(
            wake_registry.clone(),
            config.memory_janitor.interval_secs,
        );
        cortex_handles.push(janitor_handle);
        tracing::info!(
            interval_secs = config.memory_janitor.interval_secs,
            "memory janitor started"
        );
    }

    // Create cortex chat sessions for each agent
    {
        let mut sessions = std::collections::HashMap::new();
        for (agent_id, agent) in agents.iter() {
            let browser_config = (**agent.deps.runtime_config.browser_config.load()).clone();
            let brave_search_key = (**agent.deps.runtime_config.brave_search_key.load()).clone();
            let conversation_logger =
                spacebot::conversation::history::ConversationLogger::new(agent.db.sqlite.clone());
            let channel_store = spacebot::conversation::ChannelStore::new(agent.db.sqlite.clone());
            let run_logger = spacebot::conversation::ProcessRunLogger::new(agent.db.sqlite.clone());
            let cortex_ctx = spacebot::agent::cortex_chat::CortexChatSession::create_context();
            #[allow(deprecated)] // Cortex chat is legacy — being replaced by Channel Settings
            let tool_server = spacebot::tools::create_cortex_chat_tool_server(
                agent.deps.agent_id.clone(),
                agent.deps.clone(),
                agent.deps.task_store.clone(),
                agent.deps.memory_search.clone(),
                agent.deps.memory_event_tx.clone(),
                conversation_logger,
                channel_store,
                run_logger,
                browser_config,
                agent.config.screenshot_dir(),
                brave_search_key,
                agent.deps.runtime_config.workspace_dir.clone(),
                agent.deps.sandbox.clone(),
                agent.deps.runtime_config.clone(),
                api_state.clone(),
                Some(cortex_ctx.clone()),
            );
            // Add factory tools to the cortex chat tool server
            let factory_enabled = match spacebot::tools::add_factory_tools(
                &tool_server,
                api_state.clone(),
                agent.deps.memory_search.clone(),
            )
            .await
            {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%error, agent_id = %agent_id, "failed to add factory tools to cortex chat");
                    false
                }
            };

            let store = spacebot::agent::cortex_chat::CortexChatStore::new(agent.db.sqlite.clone());
            let session = spacebot::agent::cortex_chat::CortexChatSession::new(
                agent.deps.clone(),
                tool_server,
                store,
                cortex_ctx,
            )
            .with_factory(factory_enabled);
            let session = std::sync::Arc::new(session);
            session.start_event_loop();
            sessions.insert(agent_id.to_string(), session);
        }
        api_state.set_cortex_chat_sessions(sessions);
        tracing::info!("cortex chat sessions initialized");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ActiveChannelKey, queue_deferred_injection, wait_for_startup_warmup_tasks};
    use chrono::Utc;
    use spacebot::{ChannelInjection, InboundMessage, MessageContent};
    use std::collections::HashMap;
    use std::future::pending;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn startup_warmup_wait_returns_false_when_tasks_finish_in_time() {
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async {});
        let timed_out = wait_for_startup_warmup_tasks(&mut tasks, Duration::from_millis(50)).await;
        assert!(!timed_out);
    }

    #[tokio::test]
    async fn startup_warmup_wait_returns_true_when_timeout_expires() {
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
        let timed_out = wait_for_startup_warmup_tasks(&mut tasks, Duration::from_millis(5)).await;
        assert!(timed_out);
    }

    #[tokio::test]
    async fn startup_warmup_wait_aborts_timed_out_task_and_releases_lock() {
        let warmup_lock = Arc::new(tokio::sync::Mutex::new(()));
        let mut tasks = tokio::task::JoinSet::new();
        let warmup_lock_for_task = Arc::clone(&warmup_lock);
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let _guard = warmup_lock_for_task.lock().await;
            locked_tx.send(()).ok();
            pending::<()>().await;
        });

        tokio::time::timeout(Duration::from_millis(50), locked_rx)
            .await
            .expect("task should acquire lock")
            .expect("lock signal should send");

        let timed_out = wait_for_startup_warmup_tasks(&mut tasks, Duration::from_millis(5)).await;
        assert!(timed_out);

        let _guard = tokio::time::timeout(Duration::from_millis(50), warmup_lock.lock())
            .await
            .expect("startup warmup timeout should cancel blocked task and release lock");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_warmup_wait_timeout_stays_bounded_for_non_cooperative_task() {
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async {
            std::thread::sleep(Duration::from_millis(100));
        });

        let started = std::time::Instant::now();
        let timed_out = wait_for_startup_warmup_tasks(&mut tasks, Duration::from_millis(5)).await;
        assert!(timed_out);
        assert!(
            started.elapsed() < Duration::from_millis(80),
            "startup warmup timeout should return without waiting for non-cooperative task"
        );
    }

    #[test]
    fn deferred_injections_are_scoped_to_exact_agent_and_channel() {
        let mut deferred_injections: HashMap<ActiveChannelKey, Vec<InboundMessage>> =
            HashMap::new();
        let injection = ChannelInjection {
            conversation_id: "discord:dm:42".to_string(),
            agent_id: "agent-a".to_string(),
            message: InboundMessage {
                id: "inj-1".to_string(),
                source: "system".to_string(),
                adapter: None,
                conversation_id: "discord:dm:42".to_string(),
                sender_id: "system".to_string(),
                agent_id: Some(Arc::from("agent-a")),
                content: MessageContent::Text("secret cron output".to_string()),
                timestamp: Utc::now(),
                metadata: HashMap::new(),
                formatted_author: None,
            },
        };

        queue_deferred_injection(&mut deferred_injections, injection);

        assert_eq!(
            deferred_injections
                .get(&ActiveChannelKey::new("agent-a", "discord:dm:42"))
                .map(Vec::len),
            Some(1)
        );
        assert!(
            !deferred_injections.contains_key(&ActiveChannelKey::new("agent-a", "discord:123:456"))
        );
        assert!(
            !deferred_injections.contains_key(&ActiveChannelKey::new("agent-b", "discord:dm:42"))
        );
    }
}
