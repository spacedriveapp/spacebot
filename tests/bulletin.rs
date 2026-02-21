//! End-to-end test for cortex bulletin generation.
//!
//! Runs against the real ~/.spacebot data directory. Requires:
//! - ~/.spacebot/config.toml with valid LLM credentials
//! - At least one agent with memories in its database
//!
//! Run with: cargo test --test bulletin -- --nocapture

use anyhow::Context as _;
use std::sync::Arc;

/// Bootstrap an AgentDeps from the real ~/.spacebot config, using the first
/// (default) agent's databases and config.
async fn bootstrap_deps() -> anyhow::Result<spacebot::AgentDeps> {
    let config =
        spacebot::config::Config::load().context("failed to load ~/.spacebot/config.toml")?;

    let llm_manager = Arc::new(
        spacebot::llm::LlmManager::new(config.llm.clone())
            .await
            .context("failed to init LLM manager")?,
    );

    let embedding_cache_dir = config.instance_dir.join("embedding_cache");
    let embedding_model = Arc::new(
        spacebot::memory::EmbeddingModel::new(&embedding_cache_dir)
            .context("failed to init embedding model")?,
    );

    let resolved_agents = config.resolve_agents();
    let agent_config = resolved_agents.first().context("no agents configured")?;

    let db = spacebot::db::Db::connect(&agent_config.data_dir)
        .await
        .context("failed to connect databases")?;

    let memory_store = spacebot::memory::MemoryStore::new(db.sqlite.clone());

    let embedding_table = spacebot::memory::EmbeddingTable::open_or_create(&db.lance)
        .await
        .context("failed to init embedding table")?;

    if let Err(error) = embedding_table.ensure_fts_index().await {
        eprintln!("warning: FTS index creation failed: {error}");
    }

    let memory_search = Arc::new(spacebot::memory::MemorySearch::new(
        memory_store,
        embedding_table,
        embedding_model,
    ));

    let identity = spacebot::identity::Identity::load(&agent_config.workspace).await;
    let prompts =
        spacebot::prompts::PromptEngine::new("en").context("failed to init prompt engine")?;
    let skills =
        spacebot::skills::SkillSet::load(&config.skills_dir(), &agent_config.skills_dir()).await;

    let runtime_config = Arc::new(spacebot::config::RuntimeConfig::new(
        &config.instance_dir,
        agent_config,
        &config.defaults,
        prompts,
        identity,
        skills,
    ));

    let (event_tx, _) = tokio::sync::broadcast::channel(16);

    let agent_id: spacebot::AgentId = Arc::from(agent_config.id.as_str());
    let mcp_manager = Arc::new(spacebot::mcp::McpManager::new(agent_config.mcp.clone()));

    Ok(spacebot::AgentDeps {
        agent_id,
        memory_search,
        llm_manager,
        mcp_manager,
        cron_tool: None,
        runtime_config,
        event_tx,
        sqlite_pool: db.sqlite.clone(),
        messaging_manager: None,
    })
}

/// The cortex user prompt references memory types inline. If a new variant is
/// added to MemoryType::ALL, this test fails until the type list is updated.
#[test]
fn test_bulletin_prompts_cover_all_memory_types() {
    // The cortex user prompt in cortex.rs lists types inline. Check the same
    // set against the canonical list so drift is caught at compile time.
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

    // Also verify the hardcoded list matches ALL (catches additions to the
    // prompt that don't exist in the enum).
    assert_eq!(
        cortex_user_prompt_types.len(),
        spacebot::memory::types::MemoryType::ALL.len(),
        "cortex user prompt type count doesn't match MemoryType::ALL"
    );
}

#[tokio::test]
async fn test_memory_recall_returns_results() {
    let deps = bootstrap_deps().await.expect("failed to bootstrap");

    let config = spacebot::memory::search::SearchConfig::default();
    let results = deps
        .memory_search
        .hybrid_search("identity", &config)
        .await
        .expect("hybrid_search failed");

    println!("hybrid_search returned {} results", results.len());
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
        "hybrid_search should return results from a populated database"
    );
}

#[tokio::test]
async fn test_bulletin_generation() {
    let deps = bootstrap_deps().await.expect("failed to bootstrap");

    // Verify the bulletin starts empty
    let before = deps.runtime_config.memory_bulletin.load();
    assert!(before.is_empty(), "bulletin should start empty");

    // Generate the bulletin
    let logger = spacebot::agent::cortex::CortexLogger::new(deps.sqlite_pool.clone());
    let success = spacebot::agent::cortex::generate_bulletin(&deps, &logger).await;
    assert!(success, "bulletin generation should succeed");

    // Verify the bulletin was stored
    let bulletin = deps.runtime_config.memory_bulletin.load();
    assert!(
        !bulletin.is_empty(),
        "bulletin should not be empty after generation"
    );

    let word_count = bulletin.split_whitespace().count();
    println!("bulletin generated: {word_count} words");
    println!("---");
    println!("{bulletin}");
    println!("---");

    assert!(
        word_count > 50,
        "bulletin should have meaningful content (got {word_count} words)"
    );
}
