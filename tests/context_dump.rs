//! Context composition inspection test.
//!
//! Bootstraps a hermetic temp instance and dumps the full context each process
//! type sees — system prompt plus tool definitions. This is the complete
//! picture of what gets sent to the LLM on every turn, without depending on a
//! real `~/.spacebot` instance.
//!
//! Run with: cargo test --test context_dump -- --nocapture

use anyhow::Context as _;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use serde_json::Value;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const TEST_CORTEX_MODEL: &str = "test/mock-context-dump";
const TEST_BULLETIN_TEXT: &str = "Victor is the user behind this hermetic test instance. The branch surface exposes knowledge_recall before worker-only retrieval fallback, the channel stays lean, and bulletin synthesis must stay local to the test harness. Native memory remains the seeded source of truth here so context-dump inspections can prove tool visibility and prompt assembly without touching any personal machine state.";

#[derive(Clone)]
struct MockResponsesState {
    response_body: Arc<Value>,
}

struct ContextDumpHarness {
    _instance_dir: tempfile::TempDir,
    deps: spacebot::AgentDeps,
    server_task: tokio::task::JoinHandle<()>,
}

struct PreparedMockServer {
    app: axum::Router,
    listener: tokio::net::TcpListener,
    addr: SocketAddr,
}

impl PreparedMockServer {
    fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(error) = axum::serve(self.listener, self.app).await {
                tracing::debug!(%error, "mock responses server stopped");
            }
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum ContextDumpBootstrapError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    #[error("injected bootstrap failure before server spawn ({addr})")]
    InjectedBeforeServerSpawn { addr: SocketAddr },
}

impl Drop for ContextDumpHarness {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

async fn mock_responses_handler(
    State(state): State<MockResponsesState>,
    Json(_request): Json<Value>,
) -> Json<Value> {
    Json((*state.response_body).clone())
}

async fn prepare_mock_server(response_body: Arc<Value>) -> anyhow::Result<PreparedMockServer> {
    let app = axum::Router::new()
        .route("/v1/responses", post(mock_responses_handler))
        .with_state(MockResponsesState { response_body });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind mock responses server")?;
    let addr = listener
        .local_addr()
        .context("failed to read mock responses server address")?;

    Ok(PreparedMockServer {
        app,
        listener,
        addr,
    })
}

fn test_llm_config(base_url: &str) -> spacebot::config::LlmConfig {
    let mut providers = HashMap::new();
    providers.insert(
        "test".to_string(),
        spacebot::config::ProviderConfig {
            api_type: spacebot::config::ApiType::OpenAiResponses,
            base_url: base_url.to_string(),
            api_key: "test-key".to_string(),
            name: Some("Hermetic Test Provider".to_string()),
            use_bearer_auth: false,
            extra_headers: Vec::new(),
        },
    );

    spacebot::config::LlmConfig {
        anthropic_key: None,
        openai_key: None,
        openrouter_key: None,
        kilo_key: None,
        zhipu_key: None,
        groq_key: None,
        together_key: None,
        fireworks_key: None,
        deepseek_key: None,
        xai_key: None,
        mistral_key: None,
        gemini_key: None,
        ollama_key: None,
        ollama_base_url: None,
        opencode_zen_key: None,
        opencode_go_key: None,
        nvidia_key: None,
        minimax_key: None,
        minimax_cn_key: None,
        moonshot_key: None,
        zai_coding_plan_key: None,
        providers,
    }
}

fn resolved_agent_config(instance_dir: &std::path::Path) -> spacebot::config::ResolvedAgentConfig {
    let agent_root = instance_dir.join("agents").join("main");
    let routing = spacebot::llm::routing::RoutingConfig {
        cortex: TEST_CORTEX_MODEL.to_string(),
        ..Default::default()
    };

    spacebot::config::ResolvedAgentConfig {
        id: "main".to_string(),
        display_name: None,
        role: None,
        gradient_start: None,
        gradient_end: None,
        workspace: agent_root.join("workspace"),
        identity_dir: agent_root.clone(),
        data_dir: agent_root.join("data"),
        archives_dir: agent_root.join("archives"),
        routing,
        max_concurrent_branches: 2,
        max_concurrent_workers: 2,
        max_turns: 5,
        branch_max_turns: 10,
        context_window: 128_000,
        compaction: spacebot::config::CompactionConfig::default(),
        memory_persistence: spacebot::config::MemoryPersistenceConfig::default(),
        coalesce: spacebot::config::CoalesceConfig::default(),
        ingestion: spacebot::config::IngestionConfig::default(),
        cortex: spacebot::config::CortexConfig::default(),
        warmup: spacebot::config::WarmupConfig::default(),
        browser: spacebot::config::BrowserConfig::default(),
        channel: spacebot::config::ChannelConfig::default(),
        mcp: Vec::new(),
        brave_search_key: None,
        cron_timezone: None,
        user_timezone: None,
        sandbox: spacebot::sandbox::SandboxConfig::default(),
        projects: spacebot::config::ProjectsConfig::default(),
        history_backfill_count: 50,
        cron: Vec::new(),
    }
}

async fn save_memory_fixture(
    memory_store: &spacebot::memory::MemoryStore,
    memory: spacebot::memory::Memory,
) -> anyhow::Result<()> {
    memory_store.save(&memory).await?;
    Ok(())
}

async fn seed_memory_fixtures(memory_store: &spacebot::memory::MemoryStore) -> anyhow::Result<()> {
    let memories = vec![
        spacebot::memory::Memory::new(
            "Victor is the user and expects direct engineering answers with minimal fluff.",
            spacebot::memory::MemoryType::Identity,
        ),
        spacebot::memory::Memory::new(
            "Branches should prefer knowledge_recall before falling back to worker-only retrieval.",
            spacebot::memory::MemoryType::Decision,
        ),
        spacebot::memory::Memory::new(
            "Hermetic tests are preferred over any fixture that depends on ~/.spacebot.",
            spacebot::memory::MemoryType::Preference,
        ),
        spacebot::memory::Memory::new(
            "The active goal is to inspect context assembly without personal machine state.",
            spacebot::memory::MemoryType::Goal,
        ),
        spacebot::memory::Memory::new(
            "Verification found that context_dump used the real default instance and had to be isolated.",
            spacebot::memory::MemoryType::Event,
        ),
        spacebot::memory::Memory::new(
            "Native memory plus a mock bulletin response are enough to verify prompt assembly here.",
            spacebot::memory::MemoryType::Observation,
        ),
        spacebot::memory::Memory::new(
            "Victor keeps design notes in the knowledge plane and expects provenance-aware retrieval.",
            spacebot::memory::MemoryType::Fact,
        )
        .with_importance(0.9),
    ];

    for memory in memories {
        save_memory_fixture(memory_store, memory).await?;
    }

    Ok(())
}

async fn bootstrap_harness_inner(
    inject_failure_before_server_spawn: bool,
) -> Result<ContextDumpHarness, ContextDumpBootstrapError> {
    let instance_dir = tempfile::tempdir().context("failed to create temp instance dir")?;
    let agent_config = resolved_agent_config(instance_dir.path());
    std::fs::create_dir_all(&agent_config.workspace).context("failed to create workspace dir")?;
    std::fs::create_dir_all(&agent_config.data_dir).context("failed to create data dir")?;

    let response_body = Arc::new(serde_json::json!({
        "output": [
            {
                "type": "message",
                "content": [
                    {
                        "type": "output_text",
                        "text": TEST_BULLETIN_TEXT
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": 128,
            "output_tokens": 64,
            "input_tokens_details": {
                "cached_tokens": 0
            }
        }
    }));
    let prepared_server = prepare_mock_server(response_body).await?;
    let llm_manager = Arc::new(
        spacebot::llm::LlmManager::new(test_llm_config(&format!(
            "http://{}",
            prepared_server.addr
        )))
        .await
        .context("failed to init hermetic llm manager")?,
    );

    let db = spacebot::db::Db::connect(&agent_config.data_dir)
        .await
        .context("failed to connect hermetic databases")?;
    let memory_store = spacebot::memory::MemoryStore::new(db.sqlite.clone());
    let embedding_table = spacebot::memory::EmbeddingTable::open_or_create(&db.lance)
        .await
        .context("failed to init embedding table")?;

    seed_memory_fixtures(&memory_store).await?;

    let memory_search = Arc::new(spacebot::memory::MemorySearch::new_without_embeddings(
        memory_store,
        embedding_table,
    ));
    let task_store = Arc::new(spacebot::tasks::TaskStore::new(db.sqlite.clone()));
    let defaults = spacebot::config::DefaultsConfig::default();
    let runtime_config = Arc::new(spacebot::config::RuntimeConfig::new(
        instance_dir.path(),
        &agent_config,
        &defaults,
        spacebot::prompts::PromptEngine::new("en").context("failed to init prompt engine")?,
        spacebot::identity::Identity::default(),
        spacebot::skills::SkillSet::default(),
    ));

    let (event_tx, memory_event_tx) = spacebot::create_process_event_buses_with_capacity(16, 32);
    let sandbox_config = Arc::new(arc_swap::ArcSwap::from_pointee(
        agent_config.sandbox.clone(),
    ));
    let sandbox = Arc::new(
        spacebot::sandbox::Sandbox::new(
            sandbox_config,
            agent_config.workspace.clone(),
            instance_dir.path(),
            agent_config.data_dir.clone(),
        )
        .await,
    );

    let deps = spacebot::AgentDeps {
        agent_id: Arc::from(agent_config.id.as_str()),
        memory_search,
        llm_manager,
        mcp_manager: Arc::new(spacebot::mcp::McpManager::new(Vec::new())),
        task_store,
        project_store: Arc::new(spacebot::projects::ProjectStore::new(db.sqlite.clone())),
        cron_tool: None,
        runtime_config,
        event_tx,
        memory_event_tx,
        sqlite_pool: db.sqlite.clone(),
        messaging_manager: None,
        sandbox,
        links: Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
        agent_names: Arc::new(std::collections::HashMap::new()),
        humans: Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
        task_store_registry: Arc::new(arc_swap::ArcSwap::from_pointee(
            std::collections::HashMap::new(),
        )),
        process_control_registry: Arc::new(
            spacebot::agent::process_control::ProcessControlRegistry::new(),
        ),
        injection_tx: tokio::sync::mpsc::channel(1).0,
    };

    if inject_failure_before_server_spawn {
        return Err(ContextDumpBootstrapError::InjectedBeforeServerSpawn {
            addr: prepared_server.addr,
        });
    }

    let server_task = prepared_server.spawn();

    Ok(ContextDumpHarness {
        _instance_dir: instance_dir,
        deps,
        server_task,
    })
}

async fn bootstrap_harness() -> anyhow::Result<ContextDumpHarness> {
    bootstrap_harness_inner(false).await.map_err(Into::into)
}

/// Print a labeled section with a separator.
fn print_section(label: &str, content: &str) {
    let separator = "=".repeat(80);
    println!("\n{separator}");
    println!("  {label}");
    println!("{separator}\n");
    println!("{content}");
}

/// Print a char/token estimate for a prompt.
fn print_stats(label: &str, content: &str) {
    let chars = content.len();
    let words = content.split_whitespace().count();
    let estimated_tokens = chars / 4;
    println!("--- {label}: {chars} chars, {words} words, ~{estimated_tokens} tokens ---");
}

fn has_tool_named(defs: &[rig::completion::ToolDefinition], name: &str) -> bool {
    defs.iter().any(|tool| tool.name == name)
}

async fn get_branch_tool_defs() -> anyhow::Result<Vec<rig::completion::ToolDefinition>> {
    let harness = bootstrap_harness().await?;
    let deps = &harness.deps;
    let conversation_logger =
        spacebot::conversation::ConversationLogger::new(deps.sqlite_pool.clone());
    let channel_store = spacebot::conversation::ChannelStore::new(deps.sqlite_pool.clone());
    let run_logger = spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone());

    let branch_tool_server = spacebot::tools::create_branch_tool_server(
        None,
        deps.agent_id.clone(),
        deps.task_store.clone(),
        deps.memory_search.clone(),
        deps.runtime_config.clone(),
        Some(deps.mcp_manager.clone()),
        deps.memory_event_tx.clone(),
        conversation_logger,
        channel_store,
        run_logger,
    );

    branch_tool_server
        .get_tool_defs(None)
        .await
        .context("failed to get branch tool defs")
}

async fn get_channel_tool_defs() -> anyhow::Result<Vec<rig::completion::ToolDefinition>> {
    let harness = bootstrap_harness().await?;
    let deps = &harness.deps;
    let (response_tx, _response_rx) = tokio::sync::mpsc::channel(16);
    let channel_id: spacebot::ChannelId = Arc::from("test-channel");
    let state = spacebot::agent::channel::ChannelState {
        channel_id,
        history: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        active_branches: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_handles: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        active_workers: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_task_presets: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_inputs: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_injections: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        reserved_tasks: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
        status_block: Arc::new(tokio::sync::RwLock::new(
            spacebot::agent::status::StatusBlock::new(),
        )),
        deps: deps.clone(),
        conversation_logger: spacebot::conversation::ConversationLogger::new(
            deps.sqlite_pool.clone(),
        ),
        process_run_logger: spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone()),
        channel_store: spacebot::conversation::ChannelStore::new(deps.sqlite_pool.clone()),
        screenshot_dir: std::path::PathBuf::from("/tmp/screenshots"),
        logs_dir: std::path::PathBuf::from("/tmp/logs"),
        reply_target_message_id: Arc::new(tokio::sync::RwLock::new(None)),
    };

    let tool_server = rig::tool::server::ToolServer::new().run();
    let skip_flag = spacebot::tools::new_skip_flag();
    let replied_flag = spacebot::tools::new_replied_flag();
    spacebot::tools::add_channel_tools(
        &tool_server,
        state,
        response_tx,
        "test",
        skip_flag,
        replied_flag,
        None,
        None,
        true,
    )
    .await
    .context("failed to add channel tools")?;

    tool_server
        .get_tool_defs(None)
        .await
        .context("failed to get channel tool defs")
}

/// Format tool definitions as the LLM sees them.
fn format_tool_defs(defs: &[rig::completion::ToolDefinition]) -> String {
    let mut output = String::new();
    for (index, def) in defs.iter().enumerate() {
        if index > 0 {
            output.push_str("\n---\n\n");
        }
        output.push_str(&format!("### {}\n\n", def.name));
        output.push_str(&format!("{}\n\n", def.description));
        output.push_str(&format!(
            "Parameters:\n```json\n{}\n```\n",
            serde_json::to_string_pretty(&def.parameters).unwrap_or_else(|_| "{}".into())
        ));
    }
    output
}

/// Build the channel system prompt (mirrors Channel::build_system_prompt).
fn build_channel_system_prompt(rc: &spacebot::config::RuntimeConfig) -> String {
    let prompt_engine = rc.prompts.load();
    let identity_context = rc.identity.load().render();
    let memory_bulletin = rc.memory_bulletin.load();
    let skills = rc.skills.load();
    let skills_prompt = skills
        .render_channel_prompt(&prompt_engine)
        .unwrap_or_default();

    let browser_enabled = rc.browser_config.load().enabled;
    let web_search_enabled = rc.brave_search_key.load().is_some();
    let opencode_enabled = rc.opencode.load().enabled;
    let worker_capabilities = prompt_engine
        .render_worker_capabilities(browser_enabled, web_search_enabled, opencode_enabled, &[])
        .expect("failed to render worker capabilities");

    let conversation_context = prompt_engine
        .render_conversation_context("discord", Some("Test Server"), Some("#general"))
        .ok();

    let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };

    prompt_engine
        .render_channel_prompt(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            empty_to_none(skills_prompt),
            worker_capabilities,
            conversation_context,
            None,
            None,
            None,
            false,
        )
        .expect("failed to render channel prompt")
}

#[tokio::test]
async fn branch_tool_defs_include_knowledge_recall() {
    let tool_defs = get_branch_tool_defs()
        .await
        .expect("failed to build branch tool defs");
    assert!(
        has_tool_named(&tool_defs, "knowledge_recall"),
        "branch tool defs should include knowledge_recall"
    );
}

#[tokio::test]
async fn channel_tool_defs_do_not_include_knowledge_recall() {
    let tool_defs = get_channel_tool_defs()
        .await
        .expect("failed to build channel tool defs");
    assert!(
        !has_tool_named(&tool_defs, "knowledge_recall"),
        "channel tool defs should not include knowledge_recall"
    );
}

#[tokio::test]
async fn bootstrap_harness_failure_before_server_spawn_drops_listener() {
    let error = match bootstrap_harness_inner(true).await {
        Ok(_) => panic!("bootstrap should fail before server spawn"),
        Err(error) => error,
    };

    let ContextDumpBootstrapError::InjectedBeforeServerSpawn { addr } = error else {
        panic!("expected injected pre-spawn failure");
    };

    let connect_result = tokio::time::timeout(
        Duration::from_millis(250),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .expect("connect should not hang");
    assert!(
        connect_result.is_err(),
        "listener should be dropped when bootstrap fails before server spawn"
    );
}

// ─── Channel Context ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dump_channel_context() {
    let harness = bootstrap_harness().await.expect("failed to bootstrap");
    let deps = &harness.deps;
    let rc = &deps.runtime_config;

    let prompt = build_channel_system_prompt(rc);
    print_section("CHANNEL SYSTEM PROMPT", &prompt);
    print_stats("System prompt", &prompt);

    // Build the actual channel tool server with real tools registered
    let conversation_logger =
        spacebot::conversation::ConversationLogger::new(deps.sqlite_pool.clone());
    let channel_store = spacebot::conversation::ChannelStore::new(deps.sqlite_pool.clone());
    let channel_id: spacebot::ChannelId = Arc::from("test-channel");
    let status_block = Arc::new(tokio::sync::RwLock::new(
        spacebot::agent::status::StatusBlock::new(),
    ));
    let (response_tx, _response_rx) = tokio::sync::mpsc::channel(16);

    let state = spacebot::agent::channel::ChannelState {
        channel_id,
        history: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        active_branches: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_handles: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        active_workers: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_task_presets: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_inputs: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_injections: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        reserved_tasks: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
        status_block,
        deps: deps.clone(),
        conversation_logger,
        process_run_logger: spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone()),
        channel_store,
        screenshot_dir: std::path::PathBuf::from("/tmp/screenshots"),
        logs_dir: std::path::PathBuf::from("/tmp/logs"),
        reply_target_message_id: Arc::new(tokio::sync::RwLock::new(None)),
    };

    let tool_server = rig::tool::server::ToolServer::new().run();
    let skip_flag = spacebot::tools::new_skip_flag();
    let replied_flag = spacebot::tools::new_replied_flag();
    spacebot::tools::add_channel_tools(
        &tool_server,
        state,
        response_tx,
        "test-conversation",
        skip_flag,
        replied_flag,
        None,
        None,
        true,
    )
    .await
    .expect("failed to add channel tools");

    let tool_defs = tool_server
        .get_tool_defs(None)
        .await
        .expect("failed to get tool defs");

    let tools_text = format_tool_defs(&tool_defs);
    print_section(
        &format!("CHANNEL TOOLS ({} tools)", tool_defs.len()),
        &tools_text,
    );
    print_stats("Tool definitions", &tools_text);

    // Total context estimate
    let total = prompt.len() + tools_text.len();
    println!("\n--- TOTAL CHANNEL CONTEXT: ~{} tokens ---", total / 4);

    let routing = rc.routing.load();
    println!(
        "Model: {}",
        routing.resolve(spacebot::ProcessType::Channel, None)
    );
    println!("Max turns: {}", **rc.max_turns.load());

    assert!(!prompt.is_empty());
    assert!(!tool_defs.is_empty());
}

// ─── Branch Context ──────────────────────────────────────────────────────────

#[tokio::test]
async fn dump_branch_context() {
    let harness = bootstrap_harness().await.expect("failed to bootstrap");
    let deps = &harness.deps;
    let rc = &deps.runtime_config;

    let prompt_engine = rc.prompts.load();
    let instance_dir = rc.instance_dir.to_string_lossy();
    let workspace_dir = rc.workspace_dir.to_string_lossy();
    let branch_prompt = prompt_engine
        .render_branch_prompt(&instance_dir, &workspace_dir)
        .expect("failed to render branch prompt");
    print_section("BRANCH SYSTEM PROMPT", &branch_prompt);
    print_stats("System prompt", &branch_prompt);

    // Build the actual branch tool server
    let conversation_logger =
        spacebot::conversation::ConversationLogger::new(deps.sqlite_pool.clone());
    let channel_store = spacebot::conversation::ChannelStore::new(deps.sqlite_pool.clone());
    let run_logger = spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone());
    let branch_tool_server = spacebot::tools::create_branch_tool_server(
        None,
        deps.agent_id.clone(),
        deps.task_store.clone(),
        deps.memory_search.clone(),
        deps.runtime_config.clone(),
        Some(deps.mcp_manager.clone()),
        deps.memory_event_tx.clone(),
        conversation_logger,
        channel_store,
        run_logger,
    );

    let tool_defs = branch_tool_server
        .get_tool_defs(None)
        .await
        .expect("failed to get tool defs");

    let tools_text = format_tool_defs(&tool_defs);
    print_section(
        &format!("DETACHED BRANCH TOOLS ({} tools)", tool_defs.len()),
        &tools_text,
    );
    print_stats("Tool definitions", &tools_text);

    let total = branch_prompt.len() + tools_text.len();
    println!(
        "\n--- TOTAL DETACHED BRANCH CONTEXT: ~{} tokens ---",
        total / 4
    );

    let routing = rc.routing.load();
    println!(
        "Model: {}",
        routing.resolve(spacebot::ProcessType::Branch, None)
    );
    println!("Max turns: {}", **rc.branch_max_turns.load());
    println!("History: detached branch fixture (no spawn_worker registration)");

    assert!(!branch_prompt.is_empty());
    assert!(!tool_defs.is_empty());
}

// ─── Worker Context ──────────────────────────────────────────────────────────

#[tokio::test]
async fn dump_worker_context() {
    let harness = bootstrap_harness().await.expect("failed to bootstrap");
    let deps = &harness.deps;
    let rc = &deps.runtime_config;

    let prompt_engine = rc.prompts.load();
    let instance_dir = rc.instance_dir.to_string_lossy();
    let workspace_dir = rc.workspace_dir.to_string_lossy();
    let browser_config = (**rc.browser_config.load()).clone();
    let worker_prompt = prompt_engine
        .render_worker_prompt(
            &instance_dir,
            &workspace_dir,
            false,
            false,
            Vec::new(),
            Vec::new(),
            &[],
            browser_config.persist_session,
        )
        .expect("failed to render worker prompt");
    print_section("WORKER SYSTEM PROMPT", &worker_prompt);
    print_stats("System prompt", &worker_prompt);
    let brave_search_key = (**rc.brave_search_key.load()).clone();
    let worker_id = uuid::Uuid::new_v4();

    let worker_tool_server = spacebot::tools::create_worker_tool_server(
        deps.agent_id.clone(),
        worker_id,
        None,
        deps.task_store.clone(),
        deps.event_tx.clone(),
        browser_config,
        std::path::PathBuf::from("/tmp/screenshots"),
        brave_search_key,
        std::path::PathBuf::from("/tmp"),
        deps.sandbox.clone(),
        vec![],
        deps.runtime_config.clone(),
    );

    let tool_defs = worker_tool_server
        .get_tool_defs(None)
        .await
        .expect("failed to get tool defs");

    let tools_text = format_tool_defs(&tool_defs);
    print_section(
        &format!("WORKER TOOLS ({} tools)", tool_defs.len()),
        &tools_text,
    );
    print_stats("Tool definitions", &tools_text);

    let sample_task = "Search the codebase for all TODO comments and create a summary report.";
    println!("\n--- Sample task (first user message) ---");
    println!("{sample_task}");

    let total = worker_prompt.len() + tools_text.len();
    println!("\n--- TOTAL WORKER CONTEXT: ~{} tokens ---", total / 4);

    let routing = rc.routing.load();
    println!(
        "Model: {}",
        routing.resolve(spacebot::ProcessType::Worker, None)
    );
    println!("Turns per segment: 25");
    println!("History: fresh (empty). Workers have no channel context.");

    assert!(!worker_prompt.is_empty());
    assert!(!tool_defs.is_empty());
}

// ─── All Contexts Side-by-Side ───────────────────────────────────────────────

#[tokio::test]
async fn dump_all_contexts() {
    let harness = bootstrap_harness().await.expect("failed to bootstrap");
    let deps = &harness.deps;
    let rc = &deps.runtime_config;
    let prompt_engine = rc.prompts.load();
    let instance_dir = rc.instance_dir.to_string_lossy();
    let workspace_dir = rc.workspace_dir.to_string_lossy();

    // Generate bulletin so channel context is complete
    let logger = spacebot::agent::cortex::CortexLogger::new(deps.sqlite_pool.clone());
    let bulletin_success = spacebot::agent::cortex::generate_bulletin(deps, &logger).await;
    if bulletin_success {
        let bulletin = rc.memory_bulletin.load();
        println!(
            "Bulletin generated: {} words",
            bulletin.split_whitespace().count()
        );
    } else {
        println!("Bulletin generation failed (may not have memories or LLM keys)");
    }

    let conversation_logger =
        spacebot::conversation::ConversationLogger::new(deps.sqlite_pool.clone());
    let channel_store = spacebot::conversation::ChannelStore::new(deps.sqlite_pool.clone());

    // ── Channel ──
    let channel_prompt = build_channel_system_prompt(rc);

    let channel_id: spacebot::ChannelId = Arc::from("test-channel");
    let (response_tx, _response_rx) = tokio::sync::mpsc::channel(16);
    let state = spacebot::agent::channel::ChannelState {
        channel_id,
        history: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        active_branches: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_handles: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        active_workers: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_task_presets: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_inputs: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_injections: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        reserved_tasks: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
        status_block: Arc::new(tokio::sync::RwLock::new(
            spacebot::agent::status::StatusBlock::new(),
        )),
        deps: deps.clone(),
        conversation_logger: conversation_logger.clone(),
        process_run_logger: spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone()),
        channel_store: channel_store.clone(),
        screenshot_dir: std::path::PathBuf::from("/tmp/screenshots"),
        logs_dir: std::path::PathBuf::from("/tmp/logs"),
        reply_target_message_id: Arc::new(tokio::sync::RwLock::new(None)),
    };
    let channel_tool_server = rig::tool::server::ToolServer::new().run();
    let skip_flag = spacebot::tools::new_skip_flag();
    let replied_flag = spacebot::tools::new_replied_flag();
    spacebot::tools::add_channel_tools(
        &channel_tool_server,
        state,
        response_tx,
        "test",
        skip_flag,
        replied_flag,
        None,
        None,
        true,
    )
    .await
    .expect("failed to add channel tools");
    let channel_tool_defs = channel_tool_server.get_tool_defs(None).await.unwrap();
    let channel_tools_text = format_tool_defs(&channel_tool_defs);

    print_section("CHANNEL SYSTEM PROMPT (with bulletin)", &channel_prompt);
    print_stats("System prompt", &channel_prompt);
    print_section(
        &format!("CHANNEL TOOLS ({} tools)", channel_tool_defs.len()),
        &channel_tools_text,
    );
    print_stats("Tool definitions", &channel_tools_text);
    let channel_total = channel_prompt.len() + channel_tools_text.len();
    println!("--- TOTAL CHANNEL: ~{} tokens ---", channel_total / 4);

    // ── Branch ──
    let branch_prompt = prompt_engine
        .render_branch_prompt(&instance_dir, &workspace_dir)
        .expect("failed to render branch prompt");
    let run_logger = spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone());
    let branch_tool_server = spacebot::tools::create_branch_tool_server(
        None,
        deps.agent_id.clone(),
        deps.task_store.clone(),
        deps.memory_search.clone(),
        deps.runtime_config.clone(),
        Some(deps.mcp_manager.clone()),
        deps.memory_event_tx.clone(),
        conversation_logger,
        channel_store,
        run_logger,
    );
    let branch_tool_defs = branch_tool_server.get_tool_defs(None).await.unwrap();
    let branch_tools_text = format_tool_defs(&branch_tool_defs);

    print_section("BRANCH SYSTEM PROMPT", &branch_prompt);
    print_stats("System prompt", &branch_prompt);
    print_section(
        &format!("DETACHED BRANCH TOOLS ({} tools)", branch_tool_defs.len()),
        &branch_tools_text,
    );
    print_stats("Tool definitions", &branch_tools_text);
    let branch_total = branch_prompt.len() + branch_tools_text.len();
    println!(
        "--- TOTAL DETACHED BRANCH: ~{} tokens ---",
        branch_total / 4
    );

    // ── Worker ──
    let browser_config = (**rc.browser_config.load()).clone();
    let worker_prompt = prompt_engine
        .render_worker_prompt(
            &instance_dir,
            &workspace_dir,
            false,
            false,
            Vec::new(),
            Vec::new(),
            &[],
            browser_config.persist_session,
        )
        .expect("failed to render worker prompt");
    let brave_search_key = (**rc.brave_search_key.load()).clone();
    let worker_tool_server = spacebot::tools::create_worker_tool_server(
        deps.agent_id.clone(),
        uuid::Uuid::new_v4(),
        None,
        deps.task_store.clone(),
        deps.event_tx.clone(),
        browser_config,
        std::path::PathBuf::from("/tmp/screenshots"),
        brave_search_key,
        std::path::PathBuf::from("/tmp"),
        deps.sandbox.clone(),
        vec![],
        deps.runtime_config.clone(),
    );
    let worker_tool_defs = worker_tool_server.get_tool_defs(None).await.unwrap();
    let worker_tools_text = format_tool_defs(&worker_tool_defs);

    print_section("WORKER SYSTEM PROMPT", &worker_prompt);
    print_stats("System prompt", &worker_prompt);
    print_section(
        &format!("WORKER TOOLS ({} tools)", worker_tool_defs.len()),
        &worker_tools_text,
    );
    print_stats("Tool definitions", &worker_tools_text);
    let worker_total = worker_prompt.len() + worker_tools_text.len();
    println!("--- TOTAL WORKER: ~{} tokens ---", worker_total / 4);

    // ── Summary ──
    println!("\n{}", "=".repeat(80));
    println!("  SUMMARY");
    println!("{}", "=".repeat(80));

    let routing = rc.routing.load();
    println!("\nRouting:");
    println!(
        "  Channel: {}",
        routing.resolve(spacebot::ProcessType::Channel, None)
    );
    println!(
        "  Branch:  {}",
        routing.resolve(spacebot::ProcessType::Branch, None)
    );
    println!(
        "  Worker:  {}",
        routing.resolve(spacebot::ProcessType::Worker, None)
    );

    println!("\nContext budget (initial turn, before any history):");
    println!(
        "  Channel: ~{} tokens (prompt: ~{}, tools: ~{})",
        channel_total / 4,
        channel_prompt.len() / 4,
        channel_tools_text.len() / 4
    );
    println!(
        "  Branch:  ~{} tokens (prompt: ~{}, tools: ~{}) + cloned channel history",
        branch_total / 4,
        branch_prompt.len() / 4,
        branch_tools_text.len() / 4
    );
    println!(
        "  Worker:  ~{} tokens (prompt: ~{}, tools: ~{})",
        worker_total / 4,
        worker_prompt.len() / 4,
        worker_tools_text.len() / 4
    );

    let context_window = **rc.context_window.load();
    println!("\nContext window: {} tokens", context_window);
    println!(
        "  Channel headroom: ~{} tokens for history",
        context_window - channel_total / 4
    );
    println!(
        "  Branch headroom:  ~{} tokens for history",
        context_window - branch_total / 4
    );
    println!(
        "  Worker headroom:  ~{} tokens for history",
        context_window - worker_total / 4
    );

    println!("\nTurn limits:");
    println!("  Channel: {} max turns", **rc.max_turns.load());
    println!("  Branch:  {} max turns", **rc.branch_max_turns.load());
    println!("  Worker:  25 turns per segment (unlimited segments)");

    let compaction = rc.compaction.load();
    println!("\nCompaction thresholds:");
    println!(
        "  Background: {:.0}%",
        compaction.background_threshold * 100.0
    );
    println!(
        "  Aggressive: {:.0}%",
        compaction.aggressive_threshold * 100.0
    );
    println!(
        "  Emergency:  {:.0}%",
        compaction.emergency_threshold * 100.0
    );

    println!("\nTool counts:");
    println!(
        "  Channel: {} tools ({})",
        channel_tool_defs.len(),
        channel_tool_defs
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  Branch:  {} tools ({})",
        branch_tool_defs.len(),
        branch_tool_defs
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  Worker:  {} tools ({})",
        worker_tool_defs.len(),
        worker_tool_defs
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let identity = rc.identity.load();
    println!("\nIdentity files:");
    println!(
        "  SOUL.md:     {}",
        if identity.soul.is_some() {
            "loaded"
        } else {
            "empty"
        }
    );
    println!(
        "  IDENTITY.md: {}",
        if identity.identity.is_some() {
            "loaded"
        } else {
            "empty"
        }
    );
    println!(
        "  ROLE.md:     {}",
        if identity.role.is_some() {
            "loaded"
        } else {
            "empty"
        }
    );

    let skills = rc.skills.load();
    if skills.is_empty() {
        println!("\nSkills: none");
    } else {
        let skill_names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        println!("\nSkills: {}", skill_names.join(", "));
    }

    let bulletin = rc.memory_bulletin.load();
    println!(
        "\nMemory bulletin: {} words",
        bulletin.split_whitespace().count()
    );
}
