//! Spacebot CLI entry point.

use anyhow::Context as _;
use arc_swap::ArcSwap;
use clap::{Parser, Subcommand};
use futures::StreamExt as _;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "spacebot", version)]
#[command(about = "A Rust agentic system with dedicated processes for every task")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to config file (optional)
    #[arg(short, long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Enable debug logging
    #[arg(short, long, global = true)]
    debug: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon (default when no subcommand is given)
    Start {
        /// Run in the foreground instead of daemonizing
        #[arg(short, long)]
        foreground: bool,
    },
    /// Stop the running daemon
    Stop,
    /// Restart the daemon (stop + start)
    Restart {
        /// Run in the foreground instead of daemonizing
        #[arg(short, long)]
        foreground: bool,
    },
    /// Show status of the running daemon
    Status,
    /// Manage skills
    #[command(subcommand)]
    Skill(SkillCommand),
    /// Manage authentication
    #[command(subcommand)]
    Auth(AuthCommand),
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Log in to Anthropic via OAuth (opens browser)
    Login {
        /// Use API console instead of Claude Pro/Max
        #[arg(long)]
        console: bool,
    },
    /// Show current auth status
    Status,
    /// Log out (remove stored credentials)
    Logout,
    /// Refresh the access token
    Refresh,
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Install a skill from GitHub or skills.sh registry
    Add {
        /// Skill spec: owner/repo or owner/repo/skill-name
        spec: String,
        /// Agent ID to install for (defaults to first agent)
        #[arg(short, long)]
        agent: Option<String>,
        /// Install to instance-level skills directory (shared across all agents)
        #[arg(short, long)]
        instance: bool,
    },
    /// Install a skill from a .skill file
    Install {
        /// Path to .skill file
        path: std::path::PathBuf,
        /// Agent ID to install for (defaults to first agent)
        #[arg(short, long)]
        agent: Option<String>,
        /// Install to instance-level skills directory (shared across all agents)
        #[arg(short, long)]
        instance: bool,
    },
    /// List installed skills
    List {
        /// Agent ID (defaults to first agent)
        #[arg(short, long)]
        agent: Option<String>,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name
        name: String,
        /// Agent ID (defaults to first agent)
        #[arg(short, long)]
        agent: Option<String>,
    },
    /// Show skill details
    Info {
        /// Skill name
        name: String,
        /// Agent ID (defaults to first agent)
        #[arg(short, long)]
        agent: Option<String>,
    },
}

/// Tracks an active conversation channel and its message sender.
struct ActiveChannel {
    message_tx: mpsc::Sender<spacebot::InboundMessage>,
    /// Latest inbound message for this conversation, shared with the outbound
    /// routing task so status updates (e.g. typing indicators) target the
    /// most recent message rather than the first one the channel ever received.
    latest_message: Arc<tokio::sync::RwLock<spacebot::InboundMessage>>,
    /// Retained so the outbound routing task stays alive.
    _outbound_handle: tokio::task::JoinHandle<()>,
}

fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Start { foreground: false });

    match command {
        Command::Start { foreground } => cmd_start(cli.config, cli.debug, foreground),
        Command::Stop => cmd_stop(),
        Command::Restart { foreground } => {
            cmd_stop_if_running();
            cmd_start(cli.config, cli.debug, foreground)
        }
        Command::Status => cmd_status(),
        Command::Skill(skill_cmd) => cmd_skill(cli.config, skill_cmd),
        Command::Auth(auth_cmd) => cmd_auth(cli.config, auth_cmd),
    }
}

fn cmd_start(
    config_path: Option<std::path::PathBuf>,
    debug: bool,
    foreground: bool,
) -> anyhow::Result<()> {
    let paths = spacebot::daemon::DaemonPaths::from_default();

    // Bail if already running
    if let Some(pid) = spacebot::daemon::is_running(&paths) {
        eprintln!("spacebot is already running (pid {pid})");
        std::process::exit(1);
    }

    // Run onboarding interactively before daemonizing
    let resolved_config_path = if config_path.is_some() {
        config_path.clone()
    } else if spacebot::config::Config::needs_onboarding() {
        // Returns Some(path) if CLI wizard ran, None if user chose the UI.
        spacebot::config::run_onboarding().with_context(|| "onboarding failed")?
    } else {
        None
    };

    // Validate config loads successfully before forking
    let config = load_config(&resolved_config_path)?;

    if !foreground {
        // Fork the process before creating any Tokio runtime. After daemonize()
        // returns, we are in the child process — the parent has exited. Any
        // runtime created before this point would be in a broken state inside
        // the child (Tokio's I/O driver and thread pool don't survive fork),
        // which is why tracing init (and the OTLP batch exporter it creates)
        // must happen *after* this call.
        let paths = spacebot::daemon::DaemonPaths::new(&config.instance_dir);
        spacebot::daemon::daemonize(&paths)?;
    }

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

        run(config, foreground, otel_provider).await
    })
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
fn cmd_stop_if_running() {
    let paths = spacebot::daemon::DaemonPaths::from_default();

    let Some(pid) = spacebot::daemon::is_running(&paths) else {
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
            spacebot::daemon::send_command(&paths, spacebot::daemon::IpcCommand::Shutdown).await
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

fn cmd_auth(config_path: Option<std::path::PathBuf>, auth_cmd: AuthCommand) -> anyhow::Result<()> {
    // We need the instance_dir for credential storage. Try loading config,
    // but fall back to the default instance dir if config doesn't exist yet
    // (auth login may be the first thing a user runs).
    let instance_dir = if let Ok(config) = load_config(&config_path) {
        config.instance_dir
    } else {
        spacebot::config::Config::default_instance_dir()
    };

    // Ensure instance dir exists
    std::fs::create_dir_all(&instance_dir)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async {
        match auth_cmd {
            AuthCommand::Login { console } => {
                let mode = if console {
                    spacebot::auth::AuthMode::Console
                } else {
                    spacebot::auth::AuthMode::Max
                };
                spacebot::auth::login_interactive(&instance_dir, mode).await?;
                Ok(())
            }
            AuthCommand::Status => {
                match spacebot::auth::load_credentials(&instance_dir)? {
                    Some(creds) => {
                        let expires_in = creds.expires_at - chrono::Utc::now().timestamp_millis();
                        let expires_min = expires_in / 60_000;
                        if creds.is_expired() {
                            eprintln!("Anthropic OAuth: expired ({}m ago)", -expires_min);
                        } else {
                            eprintln!("Anthropic OAuth: valid (expires in {}m)", expires_min);
                        }
                        eprintln!("  access token: {}...", &creds.access_token[..20]);
                        eprintln!("  refresh token: {}...", &creds.refresh_token[..20]);
                        eprintln!(
                            "  credentials file: {}",
                            spacebot::auth::credentials_path(&instance_dir).display()
                        );
                    }
                    None => {
                        eprintln!("No OAuth credentials found.");
                        eprintln!("Run `spacebot auth login` to authenticate.");
                    }
                }
                Ok(())
            }
            AuthCommand::Logout => {
                let path = spacebot::auth::credentials_path(&instance_dir);
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    eprintln!("Credentials removed.");
                } else {
                    eprintln!("No credentials found.");
                }
                Ok(())
            }
            AuthCommand::Refresh => {
                let creds = spacebot::auth::load_credentials(&instance_dir)?
                    .context("no credentials found — run `spacebot auth login` first")?;
                eprintln!("Refreshing access token...");
                let new_creds = creds.refresh().await.context("refresh failed")?;
                spacebot::auth::save_credentials(&instance_dir, &new_creds)?;
                let expires_min =
                    (new_creds.expires_at - chrono::Utc::now().timestamp_millis()) / 60_000;
                eprintln!("Token refreshed (expires in {}m)", expires_min);
                Ok(())
            }
        }
    })
}

fn cmd_skill(
    config_path: Option<std::path::PathBuf>,
    skill_cmd: SkillCommand,
) -> anyhow::Result<()> {
    let config = load_config(&config_path)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async {
        match skill_cmd {
            SkillCommand::Add {
                spec,
                agent,
                instance,
            } => {
                let target_dir = resolve_skills_dir(&config, agent.as_deref(), instance)?;

                println!("Installing skill from: {spec}");
                println!("Target directory: {}", target_dir.display());

                let installed = spacebot::skills::install_from_github(&spec, &target_dir)
                    .await
                    .context("failed to install skill")?;

                println!("\nSuccessfully installed {} skill(s):", installed.len());
                for name in installed {
                    println!("  - {name}");
                }

                Ok(())
            }
            SkillCommand::Install {
                path,
                agent,
                instance,
            } => {
                let target_dir = resolve_skills_dir(&config, agent.as_deref(), instance)?;

                println!("Installing skill from: {}", path.display());
                println!("Target directory: {}", target_dir.display());

                let installed = spacebot::skills::install_from_file(&path, &target_dir)
                    .await
                    .context("failed to install skill")?;

                println!("\nSuccessfully installed {} skill(s):", installed.len());
                for name in installed {
                    println!("  - {name}");
                }

                Ok(())
            }
            SkillCommand::List { agent } => {
                let (instance_dir, workspace_dir) = resolve_skill_dirs(&config, agent.as_deref())?;

                let skills = spacebot::skills::SkillSet::load(&instance_dir, &workspace_dir).await;

                if skills.is_empty() {
                    println!("No skills installed");
                    return Ok(());
                }

                println!("Installed skills ({}):\n", skills.len());

                for info in skills.list() {
                    let source_label = match info.source {
                        spacebot::skills::SkillSource::Instance => "instance",
                        spacebot::skills::SkillSource::Workspace => "workspace",
                    };

                    println!("  {} ({})", info.name, source_label);
                    if !info.description.is_empty() {
                        println!("    {}", info.description);
                    }
                    println!();
                }

                Ok(())
            }
            SkillCommand::Remove { name, agent } => {
                let (instance_dir, workspace_dir) = resolve_skill_dirs(&config, agent.as_deref())?;

                let mut skills =
                    spacebot::skills::SkillSet::load(&instance_dir, &workspace_dir).await;

                match skills.remove(&name).await? {
                    Some(path) => {
                        println!("Removed skill: {name}");
                        println!("Path: {}", path.display());
                    }
                    None => {
                        eprintln!("Skill not found: {name}");
                        std::process::exit(1);
                    }
                }

                Ok(())
            }
            SkillCommand::Info { name, agent } => {
                let (instance_dir, workspace_dir) = resolve_skill_dirs(&config, agent.as_deref())?;

                let skills = spacebot::skills::SkillSet::load(&instance_dir, &workspace_dir).await;

                let Some(skill) = skills.get(&name) else {
                    eprintln!("Skill not found: {name}");
                    std::process::exit(1);
                };

                let source_label = match skill.source {
                    spacebot::skills::SkillSource::Instance => "instance",
                    spacebot::skills::SkillSource::Workspace => "workspace",
                };

                println!("Skill: {}", skill.name);
                println!("Description: {}", skill.description);
                println!("Source: {source_label}");
                println!("Path: {}", skill.file_path.display());
                println!("Base directory: {}", skill.base_dir.display());

                // Show a preview of the content
                let preview_len = skill.content.chars().take(500).count();
                if preview_len < skill.content.len() {
                    println!("\nContent preview (first 500 chars):\n");
                    println!("{}", &skill.content[..preview_len]);
                    println!(
                        "\n... ({} more characters)",
                        skill.content.len() - preview_len
                    );
                } else {
                    println!("\nContent:\n");
                    println!("{}", skill.content);
                }

                Ok(())
            }
        }
    })
}

fn resolve_skills_dir(
    config: &spacebot::config::Config,
    agent_id: Option<&str>,
    instance: bool,
) -> anyhow::Result<std::path::PathBuf> {
    if instance {
        Ok(config.skills_dir())
    } else {
        let agent_config = get_agent_config(config, agent_id)?;
        let resolved = agent_config.resolve(&config.instance_dir, &config.defaults);
        Ok(resolved.skills_dir())
    }
}

fn resolve_skill_dirs(
    config: &spacebot::config::Config,
    agent_id: Option<&str>,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    let agent_config = get_agent_config(config, agent_id)?;
    let resolved = agent_config.resolve(&config.instance_dir, &config.defaults);
    Ok((config.skills_dir(), resolved.skills_dir()))
}

fn get_agent_config<'a>(
    config: &'a spacebot::config::Config,
    agent_id: Option<&str>,
) -> anyhow::Result<&'a spacebot::config::AgentConfig> {
    let agent_id = agent_id.unwrap_or_else(|| {
        if config.agents.is_empty() {
            panic!("no agents configured");
        }
        &config.agents[0].id
    });

    config
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .with_context(|| format!("agent not found: {agent_id}"))
}

fn load_config(
    config_path: &Option<std::path::PathBuf>,
) -> anyhow::Result<spacebot::config::Config> {
    if let Some(path) = config_path {
        spacebot::config::Config::load_from_path(path)
            .with_context(|| format!("failed to load config from {}", path.display()))
    } else {
        spacebot::config::Config::load().with_context(|| "failed to load configuration")
    }
}

async fn run(
    config: spacebot::config::Config,
    foreground: bool,
    otel_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
) -> anyhow::Result<()> {
    let paths = spacebot::daemon::DaemonPaths::new(&config.instance_dir);

    tracing::info!("starting spacebot");
    tracing::info!(instance_dir = %config.instance_dir.display(), "configuration loaded");

    // Start the IPC server for stop/status commands
    let (mut shutdown_rx, _ipc_handle) = spacebot::daemon::start_ipc_server(&paths)
        .await
        .context("failed to start IPC server")?;

    // Create the provider setup channel so API handlers can signal the main loop
    let (provider_tx, mut provider_rx) = mpsc::channel::<spacebot::ProviderSetupEvent>(1);
    // Channel for newly created agents to be registered in the main event loop
    let (agent_tx, mut agent_rx) = mpsc::channel::<spacebot::Agent>(8);
    // Channel for removing agents from the main event loop
    let (agent_remove_tx, mut agent_remove_rx) = mpsc::channel::<String>(8);

    // Start HTTP API server if enabled
    let api_state = Arc::new(spacebot::api::ApiState::new_with_provider_sender(
        provider_tx,
        agent_tx,
        agent_remove_tx,
    ));

    // Start background update checker
    spacebot::update::spawn_update_checker(api_state.update_status.clone());

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
    let has_providers =
        config.llm.has_any_key() || spacebot::auth::credentials_path(&config.instance_dir).exists();

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
    let default_agent_id = config.default_agent_id().to_string();

    // Set the config path on the API state for config.toml writes
    let config_path = config.instance_dir.join("config.toml");
    api_state.set_config_path(config_path.clone()).await;
    api_state.set_llm_manager(llm_manager.clone()).await;
    api_state.set_embedding_model(embedding_model.clone()).await;
    api_state.set_prompt_engine(prompt_engine.clone()).await;
    api_state.set_defaults_config(config.defaults.clone()).await;

    // Track whether agents have been initialized
    let mut agents_initialized = false;

    // File watcher handle — started after agent init (or in setup mode with empty data)
    let mut _file_watcher;

    // If providers are available, initialize agents immediately
    if has_providers {
        let mut watcher_agents = Vec::new();
        let mut discord_permissions = None;
        let mut slack_permissions = None;
        let mut telegram_permissions = None;
        let mut twitch_permissions = None;
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
        )
        .await?;
        agents_initialized = true;

        // Start file watcher with populated agent data
        _file_watcher = spacebot::config::spawn_file_watcher(
            config_path.clone(),
            config.instance_dir.clone(),
            watcher_agents,
            discord_permissions,
            slack_permissions,
            telegram_permissions,
            twitch_permissions,
            bindings.clone(),
            Some(messaging_manager.clone()),
            llm_manager.clone(),
        );
    } else {
        // Start file watcher in setup mode (no agents to watch yet)
        _file_watcher = spacebot::config::spawn_file_watcher(
            config_path.clone(),
            config.instance_dir.clone(),
            Vec::new(),
            None,
            None,
            None,
            None,
            bindings.clone(),
            None,
            llm_manager.clone(),
        );
    }

    if foreground {
        eprintln!(
            "spacebot running in foreground (pid {})",
            std::process::id()
        );
    } else {
        tracing::info!(pid = std::process::id(), "spacebot daemon started");
    }

    // Active conversation channels: conversation_id -> ActiveChannel
    let mut active_channels: HashMap<String, ActiveChannel> = HashMap::new();

    // Main event loop: route inbound messages to agent channels
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
                let agent_id = if let Some(existing) = message.agent_id.as_ref() {
                    existing.clone()
                } else {
                    let current_bindings = bindings.load();
                    let resolved = spacebot::config::resolve_agent_for_message(
                        &current_bindings,
                        &message,
                        &default_agent_id,
                    );
                    message.agent_id = Some(resolved.clone());
                    resolved
                };

                let conversation_id = message.conversation_id.clone();

                // Find or create a channel for this conversation
                if !active_channels.contains_key(&conversation_id) {
                    let Some(agent) = agents.get(&agent_id) else {
                        tracing::warn!(
                            agent_id = %agent_id,
                            conversation_id = %conversation_id,
                            "message routed to unknown agent, dropping"
                        );
                        continue;
                    };

                    // Create outbound response channel
                    let (response_tx, mut response_rx) = mpsc::channel::<spacebot::OutboundResponse>(32);

                    // Subscribe to the agent's event bus
                    let event_rx = agent.deps.event_tx.subscribe();

                    let channel_id: spacebot::ChannelId = Arc::from(conversation_id.as_str());

                    let (channel, channel_tx) = spacebot::agent::channel::Channel::new(
                        channel_id,
                        agent.deps.clone(),
                        response_tx,
                        event_rx,
                        agent.config.screenshot_dir(),
                        agent.config.logs_dir(),
                    );

                    // Register the channel's status block with the API for snapshot queries
                    api_state.register_channel_status(
                        conversation_id.clone(),
                        channel.state.status_block.clone(),
                    ).await;

                    // Register the channel state for API-driven cancellation
                    api_state.register_channel_state(
                        conversation_id.clone(),
                        channel.state.clone(),
                    ).await;

                    // Backfill recent message history from the platform
                    let backfill_count = agent.config.history_backfill_count();
                    if backfill_count > 0 {
                        match messaging_manager.fetch_history(&message, backfill_count).await {
                            Ok(history_messages) if !history_messages.is_empty() => {
                                let mut transcript = String::new();
                                for entry in &history_messages {
                                    let label = if entry.is_bot { "(you)" } else { &entry.author };
                                    transcript.push_str(&format!("{}: {}\n", label, entry.content));
                                }

                                let prompt_engine = agent.deps.runtime_config.prompts.load();
                                let backfill_text = prompt_engine
                                    .render_system_history_backfill(transcript.trim_end())
                                    .unwrap_or(transcript);

                                let mut history = channel.state.history.write().await;
                                history.push(rig::message::Message::from(backfill_text));
                                drop(history);

                                tracing::info!(
                                    conversation_id = %conversation_id,
                                    message_count = history_messages.len(),
                                    "backfilled channel history"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(%error, "failed to backfill channel history");
                            }
                            _ => {}
                        }
                    }

                    // Spawn the channel's event loop
                    tokio::spawn(async move {
                        if let Err(error) = channel.run().await {
                            tracing::error!(%error, "channel event loop failed");
                        }
                    });

                    // Spawn outbound response routing: reads from response_rx,
                    // sends to the messaging adapter and forwards to SSE
                    let messaging_for_outbound = messaging_manager.clone();
                    let latest_message = Arc::new(tokio::sync::RwLock::new(message.clone()));
                    let outbound_message = latest_message.clone();
                    let outbound_conversation_id = conversation_id.clone();
                    let api_event_tx = api_state.event_tx.clone();
                    let sse_agent_id = agent_id.to_string();
                    let sse_channel_id = conversation_id.clone();
                    let outbound_handle = tokio::spawn(async move {
                        while let Some(response) = response_rx.recv().await {
                            // Forward relevant events to SSE clients
                            match &response {
                                spacebot::OutboundResponse::Text(text) => {
                                    api_event_tx.send(spacebot::api::ApiEvent::OutboundMessage {
                                        agent_id: sse_agent_id.clone(),
                                        channel_id: sse_channel_id.clone(),
                                        text: text.clone(),
                                    }).ok();
                                }
                                spacebot::OutboundResponse::RichMessage { text, .. } => {
                                    api_event_tx.send(spacebot::api::ApiEvent::OutboundMessage {
                                        agent_id: sse_agent_id.clone(),
                                        channel_id: sse_channel_id.clone(),
                                        text: text.clone(),
                                    }).ok();
                                }
                                spacebot::OutboundResponse::ThreadReply { text, .. } => {
                                    api_event_tx.send(spacebot::api::ApiEvent::OutboundMessage {
                                        agent_id: sse_agent_id.clone(),
                                        channel_id: sse_channel_id.clone(),
                                        text: text.clone(),
                                    }).ok();
                                }
                                spacebot::OutboundResponse::Status(spacebot::StatusUpdate::Thinking) => {
                                    api_event_tx.send(spacebot::api::ApiEvent::TypingState {
                                        agent_id: sse_agent_id.clone(),
                                        channel_id: sse_channel_id.clone(),
                                        is_typing: true,
                                    }).ok();
                                }
                                spacebot::OutboundResponse::Status(spacebot::StatusUpdate::StopTyping) => {
                                    api_event_tx.send(spacebot::api::ApiEvent::TypingState {
                                        agent_id: sse_agent_id.clone(),
                                        channel_id: sse_channel_id.clone(),
                                        is_typing: false,
                                    }).ok();
                                }
                                _ => {}
                            }

                            let current_message = outbound_message.read().await.clone();
                            match response {
                                spacebot::OutboundResponse::Status(status) => {
                                    if let Err(error) = messaging_for_outbound
                                        .send_status(&current_message, status)
                                        .await
                                    {
                                        tracing::warn!(%error, "failed to send status update");
                                    }
                                }
                                response => {
                                    tracing::info!(
                                        conversation_id = %outbound_conversation_id,
                                        "routing outbound response to messaging adapter"
                                    );
                                    if let Err(error) = messaging_for_outbound
                                        .respond(&current_message, response)
                                        .await
                                    {
                                        tracing::error!(%error, "failed to send outbound response");
                                    }
                                }
                            }
                        }
                    });

                    active_channels.insert(conversation_id.clone(), ActiveChannel {
                        message_tx: channel_tx,
                        latest_message,
                        _outbound_handle: outbound_handle,
                    });

                    tracing::info!(
                        conversation_id = %conversation_id,
                        agent_id = %agent_id,
                        "new channel created"
                    );
                }

                // Forward the message to the channel
                if let Some(active) = active_channels.get(&conversation_id) {
                    // Update the shared message reference so outbound routing
                    // (typing indicators, reactions) targets this message
                    *active.latest_message.write().await = message.clone();

                    // Emit inbound message to SSE clients
                    let sender_name = message.formatted_author.clone().or_else(|| {
                        message
                            .metadata
                            .get("sender_display_name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                    api_state.event_tx.send(spacebot::api::ApiEvent::InboundMessage {
                        agent_id: agent_id.to_string(),
                        channel_id: conversation_id.clone(),
                        sender_name,
                        sender_id: message.sender_id.clone(),
                        text: message.content.to_string(),
                    }).ok();

                    if let Err(error) = active.message_tx.send(message).await {
                        tracing::error!(
                            conversation_id = %conversation_id,
                            %error,
                            "failed to forward message to channel"
                        );
                        active_channels.remove(&conversation_id);
                    }
                }
            }
            Some(agent) = agent_rx.recv() => {
                tracing::info!(agent_id = %agent.id, "registering new agent in main loop");
                agents.insert(agent.id.clone(), agent);
            }
            Some(agent_id) = agent_remove_rx.recv() => {
                let key: spacebot::AgentId = Arc::from(agent_id.as_str());
                if let Some(agent) = agents.remove(&key) {
                    agent.deps.mcp_manager.disconnect_all().await;
                    tracing::info!(agent_id = %agent_id, "removed agent from main loop");
                } else {
                    tracing::warn!(agent_id = %agent_id, "agent not found in main loop for removal");
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
                    Ok(new_config) if new_config.llm.has_any_key() => {
                        // Rebuild LlmManager with the new keys
                        match spacebot::llm::LlmManager::new(new_config.llm.clone()).await {
                            Ok(new_llm) => {
                                let new_llm_manager = Arc::new(new_llm);
                                let mut new_watcher_agents = Vec::new();
                                let mut new_discord_permissions = None;
                                let mut new_slack_permissions = None;
                                let mut new_telegram_permissions = None;
                                let mut new_twitch_permissions = None;
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
                                ).await {
                                    Ok(()) => {
                                        agents_initialized = true;
                                        // Restart file watcher with the new agent data
                                        _file_watcher = spacebot::config::spawn_file_watcher(
                                            config_path.clone(),
                                            new_config.instance_dir.clone(),
                                            new_watcher_agents,
                                            new_discord_permissions,
                                            new_slack_permissions,
                                            new_telegram_permissions,
                                            new_twitch_permissions,
                                            bindings.clone(),
                                            Some(messaging_manager.clone()),
                                            new_llm_manager.clone(),
                                        );
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
                    }
                    Ok(_) => {
                        tracing::warn!("config reloaded but still no providers configured");
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to reload config after provider setup");
                    }
                }
            }
            _ = shutdown_rx.wait_for(|shutdown| *shutdown) => {
                tracing::info!("shutdown signal received via IPC");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown signal received");
                break;
            }
        }
    }

    // Graceful shutdown
    drop(active_channels);

    for scheduler in &cron_schedulers_for_shutdown {
        scheduler.shutdown().await;
    }
    drop(cron_schedulers_for_shutdown);

    messaging_manager.shutdown().await;

    for (agent_id, agent) in agents {
        tracing::info!(%agent_id, "shutting down agent");
        agent.deps.mcp_manager.disconnect_all().await;
        agent.db.close().await;
    }

    tracing::info!("spacebot stopped");

    // Flush buffered OTLP spans before the process exits. Without this the
    // batch exporter drops any spans recorded in the last export interval.
    if let Some(provider) = otel_provider {
        if let Err(error) = provider.shutdown() {
            tracing::warn!(%error, "failed to flush OTel spans on shutdown");
        }
    }

    spacebot::daemon::cleanup(&paths);

    // Force exit — detached tasks (e.g. the serenity gateway client) may keep
    // the tokio runtime alive after all owned resources have been cleaned up.
    std::process::exit(0);
}

/// Initialize agents, messaging adapters, cron, cortex, and ingestion.
/// Extracted so it can be called either at startup or after providers are configured.
#[allow(clippy::too_many_arguments)]
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
        Arc<spacebot::config::RuntimeConfig>,
        Arc<spacebot::mcp::McpManager>,
    )>,
    discord_permissions: &mut Option<Arc<ArcSwap<spacebot::config::DiscordPermissions>>>,
    slack_permissions: &mut Option<Arc<ArcSwap<spacebot::config::SlackPermissions>>>,
    telegram_permissions: &mut Option<Arc<ArcSwap<spacebot::config::TelegramPermissions>>>,
    twitch_permissions: &mut Option<Arc<ArcSwap<spacebot::config::TwitchPermissions>>>,
) -> anyhow::Result<()> {
    let resolved_agents = config.resolve_agents();

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
        std::fs::create_dir_all(&agent_config.ingest_dir()).with_context(|| {
            format!(
                "failed to create ingest dir: {}",
                agent_config.ingest_dir().display()
            )
        })?;
        std::fs::create_dir_all(&agent_config.logs_dir()).with_context(|| {
            format!(
                "failed to create logs dir: {}",
                agent_config.logs_dir().display()
            )
        })?;

        // Per-agent database connections
        let db = spacebot::db::Db::connect(&agent_config.data_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to connect databases for agent '{}'",
                    agent_config.id
                )
            })?;

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

        // Per-agent memory system
        let memory_store = spacebot::memory::MemoryStore::new(db.sqlite.clone());
        let embedding_table = spacebot::memory::EmbeddingTable::open_or_create(&db.lance)
            .await
            .with_context(|| {
                format!("failed to init embeddings for agent '{}'", agent_config.id)
            })?;

        // Ensure FTS index exists for full-text search queries
        if let Err(error) = embedding_table.ensure_fts_index().await {
            tracing::warn!(%error, agent = %agent_config.id, "failed to create FTS index");
        }

        let memory_search = Arc::new(spacebot::memory::MemorySearch::new(
            memory_store,
            embedding_table,
            embedding_model.clone(),
        ));

        // Per-agent event bus (broadcast for fan-out to multiple channels)
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(256);

        let agent_id: spacebot::AgentId = Arc::from(agent_config.id.as_str());
        let mcp_manager = Arc::new(spacebot::mcp::McpManager::new(agent_config.mcp.clone()));
        mcp_manager.connect_all().await;

        // Scaffold identity templates if missing, then load
        spacebot::identity::scaffold_identity_files(&agent_config.workspace)
            .await
            .with_context(|| {
                format!(
                    "failed to scaffold identity files for agent '{}'",
                    agent_config.id
                )
            })?;
        let identity = spacebot::identity::Identity::load(&agent_config.workspace).await;

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

        // Set the settings store in RuntimeConfig and apply config-driven defaults
        runtime_config.set_settings(settings_store.clone());
        if let Err(error) = settings_store.set_worker_log_mode(config.defaults.worker_log_mode) {
            tracing::warn!(%error, agent = %agent_config.id, "failed to set worker_log_mode from config");
        }

        watcher_agents.push((
            agent_config.id.clone(),
            agent_config.workspace.clone(),
            runtime_config.clone(),
            mcp_manager.clone(),
        ));

        let deps = spacebot::AgentDeps {
            agent_id: agent_id.clone(),
            memory_search,
            llm_manager: llm_manager.clone(),
            mcp_manager,
            cron_tool: None,
            runtime_config,
            event_tx,
            sqlite_pool: db.sqlite.clone(),
            messaging_manager: None,
        };

        let agent = spacebot::Agent {
            id: agent_id.clone(),
            config: agent_config.clone(),
            db,
            deps,
        };

        tracing::info!(agent_id = %agent_config.id, "agent initialized");
        agents.insert(agent_id, agent);
    }

    tracing::info!(agent_count = agents.len(), "all agents initialized");

    // Wire agent event streams, DB pools, and config summaries into the API server
    {
        let mut agent_pools = std::collections::HashMap::new();
        let mut agent_configs = Vec::new();
        let mut memory_searches = std::collections::HashMap::new();
        let mut mcp_managers = std::collections::HashMap::new();
        let mut agent_workspaces = std::collections::HashMap::new();
        let mut runtime_configs = std::collections::HashMap::new();
        for (agent_id, agent) in agents.iter() {
            let event_rx = agent.deps.event_tx.subscribe();
            api_state.register_agent_events(agent_id.to_string(), event_rx);
            agent_pools.insert(agent_id.to_string(), agent.db.sqlite.clone());
            memory_searches.insert(agent_id.to_string(), agent.deps.memory_search.clone());
            mcp_managers.insert(agent_id.to_string(), agent.deps.mcp_manager.clone());
            agent_workspaces.insert(agent_id.to_string(), agent.config.workspace.clone());
            runtime_configs.insert(agent_id.to_string(), agent.deps.runtime_config.clone());
            agent_configs.push(spacebot::api::AgentInfo {
                id: agent.config.id.clone(),
                workspace: agent.config.workspace.clone(),
                context_window: agent.config.context_window,
                max_turns: agent.config.max_turns,
                max_concurrent_branches: agent.config.max_concurrent_branches,
                max_concurrent_workers: agent.config.max_concurrent_workers,
            });
        }
        api_state.set_agent_pools(agent_pools);
        api_state.set_agent_configs(agent_configs);
        api_state.set_memory_searches(memory_searches);
        api_state.set_mcp_managers(mcp_managers);
        api_state.set_runtime_configs(runtime_configs);
        api_state.set_agent_workspaces(agent_workspaces);
        api_state.set_instance_dir(config.instance_dir.clone());
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

    if let Some(discord_config) = &config.messaging.discord {
        if discord_config.enabled {
            let adapter = spacebot::messaging::discord::DiscordAdapter::new(
                &discord_config.token,
                discord_permissions
                    .clone()
                    .expect("discord permissions initialized when discord is enabled"),
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

    if let Some(slack_config) = &config.messaging.slack {
        if slack_config.enabled {
            match spacebot::messaging::slack::SlackAdapter::new(
                &slack_config.bot_token,
                &slack_config.app_token,
                slack_permissions
                    .clone()
                    .expect("slack permissions initialized when slack is enabled"),
                slack_config.commands.clone(),
            ) {
                Ok(adapter) => {
                    new_messaging_manager.register(adapter).await;
                }
                Err(error) => {
                    tracing::error!(%error, "failed to build slack adapter");
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

    if let Some(telegram_config) = &config.messaging.telegram {
        if telegram_config.enabled {
            let adapter = spacebot::messaging::telegram::TelegramAdapter::new(
                &telegram_config.token,
                telegram_permissions
                    .clone()
                    .expect("telegram permissions initialized when telegram is enabled"),
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    if let Some(webhook_config) = &config.messaging.webhook {
        if webhook_config.enabled {
            let adapter = spacebot::messaging::webhook::WebhookAdapter::new(
                webhook_config.port,
                &webhook_config.bind,
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    // Shared Twitch permissions (hot-reloadable via file watcher)
    *twitch_permissions = config.messaging.twitch.as_ref().map(|twitch_config| {
        let perms =
            spacebot::config::TwitchPermissions::from_config(twitch_config, &config.bindings);
        Arc::new(ArcSwap::from_pointee(perms))
    });

    if let Some(twitch_config) = &config.messaging.twitch {
        if twitch_config.enabled {
            let adapter = spacebot::messaging::twitch::TwitchAdapter::new(
                &twitch_config.username,
                &twitch_config.oauth_token,
                twitch_config.channels.clone(),
                twitch_config.trigger_prefix.clone(),
                twitch_permissions
                    .clone()
                    .expect("twitch permissions initialized when twitch is enabled"),
            );
            new_messaging_manager.register(adapter).await;
        }
    }

    let webchat_adapter = Arc::new(spacebot::messaging::webchat::WebChatAdapter::new());
    new_messaging_manager
        .register_shared(webchat_adapter.clone())
        .await;
    api_state.set_webchat_adapter(webchat_adapter);

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

        // Seed cron jobs from config into the database
        for cron_def in &agent.config.cron {
            let cron_config = spacebot::cron::CronConfig {
                id: cron_def.id.clone(),
                prompt: cron_def.prompt.clone(),
                interval_secs: cron_def.interval_secs,
                delivery_target: cron_def.delivery_target.clone(),
                active_hours: cron_def.active_hours,
                enabled: cron_def.enabled,
                run_once: cron_def.run_once,
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
                for cron_config in configs {
                    if let Err(error) = scheduler.register(cron_config).await {
                        tracing::warn!(agent_id = %agent_id, %error, "failed to register cron job");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(agent_id = %agent_id, %error, "failed to load cron jobs from database");
            }
        }

        // Store cron tool on deps so each channel can register it on its own tool server
        let cron_tool = spacebot::tools::CronTool::new(store.clone(), scheduler.clone());
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

    // Start cortex bulletin loops and association loops for each agent
    for (agent_id, agent) in agents.iter() {
        let cortex_logger = spacebot::agent::cortex::CortexLogger::new(agent.db.sqlite.clone());
        let bulletin_handle =
            spacebot::agent::cortex::spawn_bulletin_loop(agent.deps.clone(), cortex_logger.clone());
        cortex_handles.push(bulletin_handle);
        tracing::info!(agent_id = %agent_id, "cortex bulletin loop started");

        let association_handle =
            spacebot::agent::cortex::spawn_association_loop(agent.deps.clone(), cortex_logger);
        cortex_handles.push(association_handle);
        tracing::info!(agent_id = %agent_id, "cortex association loop started");
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
            let tool_server = spacebot::tools::create_cortex_chat_tool_server(
                agent.deps.memory_search.clone(),
                conversation_logger,
                channel_store,
                browser_config,
                agent.config.screenshot_dir(),
                brave_search_key,
                agent.deps.runtime_config.workspace_dir.clone(),
                agent.deps.runtime_config.instance_dir.clone(),
            );
            let store = spacebot::agent::cortex_chat::CortexChatStore::new(agent.db.sqlite.clone());
            let session = spacebot::agent::cortex_chat::CortexChatSession::new(
                agent.deps.clone(),
                tool_server,
                store,
            );
            sessions.insert(agent_id.to_string(), std::sync::Arc::new(session));
        }
        api_state.set_cortex_chat_sessions(sessions);
        tracing::info!("cortex chat sessions initialized");
    }

    Ok(())
}
