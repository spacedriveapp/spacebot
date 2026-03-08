//! Hermetic end-to-end tests for cortex bulletin generation.

use anyhow::Context as _;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use serde_json::Value;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const TEST_CORTEX_MODEL: &str = "test/mock-bulletin";
const TEST_BULLETIN_TEXT: &str = "Victor is the user behind this Spacebot instance. He prefers direct answers, keeps a strong bias toward durable architecture, and tracks design notes inside the knowledge plane. Recent work introduced a native knowledge recall surface for branches, preserved worker fallback for sources that are not adapter-backed yet, and kept provenance mandatory on normalized retrieval hits. Current priorities include wiring QMD behind the native contract, validating mixed-source retrieval, and keeping bulletin generation isolated from live personal state. The system should stay hermetic in tests, rely on local fixtures, and avoid silently depending on the default ~/.spacebot instance.";

#[derive(Clone)]
struct MockResponsesState {
    response_body: Arc<Value>,
}

struct BulletinTestHarness {
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
enum BulletinBootstrapError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
    #[error("injected bootstrap failure before server spawn ({addr})")]
    InjectedBeforeServerSpawn { addr: SocketAddr },
}

impl Drop for BulletinTestHarness {
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
    let server_state = MockResponsesState { response_body };
    let app = axum::Router::new()
        .route("/v1/responses", post(mock_responses_handler))
        .with_state(server_state);
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
        workspace: agent_root.join("workspace"),
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
            "Victor is the user and wants direct, high-signal engineering answers.",
            spacebot::memory::MemoryType::Identity,
        ),
        spacebot::memory::Memory::new(
            "Victor decided the new retrieval plane should expose a native knowledge_recall tool before wiring QMD adapters.",
            spacebot::memory::MemoryType::Decision,
        ),
        spacebot::memory::Memory::new(
            "Victor prefers durable architecture over brittle shortcuts, especially in test harnesses.",
            spacebot::memory::MemoryType::Preference,
        ),
        spacebot::memory::Memory::new(
            "The active goal is to make bulletin verification hermetic and independent from ~/.spacebot.",
            spacebot::memory::MemoryType::Goal,
        ),
        spacebot::memory::Memory::new(
            "A verification run found that tests/bulletin.rs depended on the real default instance and failed on migration drift.",
            spacebot::memory::MemoryType::Event,
        ),
        spacebot::memory::Memory::new(
            "Native memory retrieval already normalizes provenance and keeps branch-visible knowledge lookups local.",
            spacebot::memory::MemoryType::Observation,
        ),
        spacebot::memory::Memory::new(
            "Victor keeps design notes in the knowledge plane and uses them to steer implementation work.",
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
) -> Result<BulletinTestHarness, BulletinBootstrapError> {
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
            "output_tokens": 96,
            "input_tokens_details": {
                "cached_tokens": 0
            }
        }
    }));

    let prepared_server = prepare_mock_server(response_body).await?;
    let llm_manager = Arc::new(
        spacebot::llm::LlmManager::new(test_llm_config(&format!("http://{}", prepared_server.addr)))
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
        return Err(BulletinBootstrapError::InjectedBeforeServerSpawn {
            addr: prepared_server.addr,
        });
    }

    let server_task = prepared_server.spawn();

    Ok(BulletinTestHarness {
        _instance_dir: instance_dir,
        deps,
        server_task,
    })
}

async fn bootstrap_harness() -> anyhow::Result<BulletinTestHarness> {
    bootstrap_harness_inner(false).await.map_err(Into::into)
}

/// The cortex user prompt references memory types inline. If a new variant is
/// added to MemoryType::ALL, this test fails until the type list is updated.
#[test]
fn test_bulletin_prompts_cover_all_memory_types() {
    let cortex_user_prompt_types = [
        "identity",
        "fact",
        "decision",
        "event",
        "preference",
        "observation",
        "goal",
        "todo",
    ];

    for memory_type in spacebot::memory::types::MemoryType::ALL {
        let type_str = memory_type.to_string();

        assert!(
            cortex_user_prompt_types.contains(&type_str.as_str()),
            "cortex user prompt is missing memory type: \"{type_str}\""
        );
    }

    assert_eq!(
        cortex_user_prompt_types.len(),
        spacebot::memory::types::MemoryType::ALL.len(),
        "cortex user prompt type count doesn't match MemoryType::ALL"
    );
}

#[tokio::test]
async fn test_bulletin_memory_sections_have_results() {
    let harness = bootstrap_harness().await.expect("failed to bootstrap");

    let config = spacebot::memory::search::SearchConfig {
        mode: spacebot::memory::search::SearchMode::Typed,
        memory_type: Some(spacebot::memory::MemoryType::Identity),
        sort_by: spacebot::memory::search::SearchSort::Recent,
        max_results: 5,
        ..Default::default()
    };
    let results = harness
        .deps
        .memory_search
        .search("", &config)
        .await
        .expect("memory search failed");

    println!("memory search returned {} results", results.len());
    for result in &results {
        println!(
            "  [{:.4}] {} — {}",
            result.score,
            result.memory.memory_type,
            result.memory.content.lines().next().unwrap_or("(empty)")
        );
    }

    assert!(
        !results.is_empty(),
        "memory search should return results from the seeded hermetic database"
    );
}

#[tokio::test]
async fn test_bulletin_generation() {
    let harness = bootstrap_harness().await.expect("failed to bootstrap");

    let before = harness.deps.runtime_config.memory_bulletin.load();
    assert!(before.is_empty(), "bulletin should start empty");

    let logger = spacebot::agent::cortex::CortexLogger::new(harness.deps.sqlite_pool.clone());
    let success = spacebot::agent::cortex::generate_bulletin(&harness.deps, &logger).await;
    assert!(success, "bulletin generation should succeed");

    let bulletin = harness.deps.runtime_config.memory_bulletin.load();
    assert!(
        !bulletin.is_empty(),
        "bulletin should not be empty after generation"
    );

    let word_count = bulletin.split_whitespace().count();
    println!("bulletin generated: {word_count} words");
    println!("---");
    println!("{bulletin}");
    println!("---");

    assert_eq!(bulletin.as_ref(), TEST_BULLETIN_TEXT);
    assert!(
        word_count > 50,
        "bulletin should have meaningful content (got {word_count} words)"
    );
}

#[tokio::test]
async fn bootstrap_harness_failure_before_server_spawn_drops_listener() {
    let error = match bootstrap_harness_inner(true).await {
        Ok(_) => panic!("bootstrap should fail before server spawn"),
        Err(error) => error,
    };

    let BulletinBootstrapError::InjectedBeforeServerSpawn { addr } = error else {
        panic!("expected injected pre-spawn failure");
    };

    let connect_result =
        tokio::time::timeout(Duration::from_millis(250), tokio::net::TcpStream::connect(addr))
            .await
            .expect("connect should not hang");
    assert!(
        connect_result.is_err(),
        "listener should be dropped when bootstrap fails before server spawn"
    );
}
