//! Context composition inspection test.
//!
//! Bootstraps from the real ~/.spacebot data directory and dumps the full
//! context each process type sees — system prompt + tool definitions (names,
//! descriptions, parameter schemas). This is the complete picture of what
//! gets sent to the LLM on every turn.
//!
//! Run with: cargo test --test context_dump -- --nocapture

use anyhow::Context as _;
use std::sync::Arc;

/// Bootstrap AgentDeps from the real ~/.spacebot config (same as bulletin test).
async fn bootstrap_deps() -> anyhow::Result<(spacebot::AgentDeps, spacebot::config::Config)> {
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

    let deps = spacebot::AgentDeps {
        agent_id,
        memory_search,
        llm_manager,
        mcp_manager,
        cron_tool: None,
        runtime_config,
        event_tx,
        sqlite_pool: db.sqlite.clone(),
        messaging_manager: None,
    };

    Ok((deps, config))
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
    let skills_prompt = skills.render_channel_prompt(&prompt_engine);

    let browser_enabled = rc.browser_config.load().enabled;
    let web_search_enabled = rc.brave_search_key.load().is_some();
    let opencode_enabled = rc.opencode.load().enabled;
    let worker_capabilities = prompt_engine
        .render_worker_capabilities(browser_enabled, web_search_enabled, opencode_enabled)
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
        )
        .expect("failed to render channel prompt")
}

// ─── Channel Context ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dump_channel_context() {
    let (deps, _config) = bootstrap_deps().await.expect("failed to bootstrap");
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
        process_run_logger: spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone()),
        active_workers: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_inputs: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        status_block,
        deps: deps.clone(),
        conversation_logger,
        channel_store,
        screenshot_dir: std::path::PathBuf::from("/tmp/screenshots"),
        logs_dir: std::path::PathBuf::from("/tmp/logs"),
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
    let (deps, _config) = bootstrap_deps().await.expect("failed to bootstrap");
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
    let branch_tool_server = spacebot::tools::create_branch_tool_server(
        deps.memory_search.clone(),
        conversation_logger,
        channel_store,
    );

    let tool_defs = branch_tool_server
        .get_tool_defs(None)
        .await
        .expect("failed to get tool defs");

    let tools_text = format_tool_defs(&tool_defs);
    print_section(
        &format!("BRANCH TOOLS ({} tools)", tool_defs.len()),
        &tools_text,
    );
    print_stats("Tool definitions", &tools_text);

    let total = branch_prompt.len() + tools_text.len();
    println!("\n--- TOTAL BRANCH CONTEXT: ~{} tokens ---", total / 4);

    let routing = rc.routing.load();
    println!(
        "Model: {}",
        routing.resolve(spacebot::ProcessType::Branch, None)
    );
    println!("Max turns: {}", **rc.branch_max_turns.load());
    println!("History: cloned from channel at fork time (full conversation context)");

    assert!(!branch_prompt.is_empty());
    assert!(!tool_defs.is_empty());
}

// ─── Worker Context ──────────────────────────────────────────────────────────

#[tokio::test]
async fn dump_worker_context() {
    let (deps, _config) = bootstrap_deps().await.expect("failed to bootstrap");
    let rc = &deps.runtime_config;

    let prompt_engine = rc.prompts.load();
    let instance_dir = rc.instance_dir.to_string_lossy();
    let workspace_dir = rc.workspace_dir.to_string_lossy();
    let worker_prompt = prompt_engine
        .render_worker_prompt(&instance_dir, &workspace_dir)
        .expect("failed to render worker prompt");
    print_section("WORKER SYSTEM PROMPT", &worker_prompt);
    print_stats("System prompt", &worker_prompt);

    // Build the actual worker tool server
    let browser_config = (**rc.browser_config.load()).clone();
    let brave_search_key = (**rc.brave_search_key.load()).clone();
    let worker_id = uuid::Uuid::new_v4();

    let worker_tool_server = spacebot::tools::create_worker_tool_server(
        deps.agent_id.clone(),
        worker_id,
        None,
        deps.event_tx.clone(),
        browser_config,
        std::path::PathBuf::from("/tmp/screenshots"),
        brave_search_key,
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/tmp"),
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
    let (deps, _config) = bootstrap_deps().await.expect("failed to bootstrap");
    let rc = &deps.runtime_config;
    let prompt_engine = rc.prompts.load();
    let instance_dir = rc.instance_dir.to_string_lossy();
    let workspace_dir = rc.workspace_dir.to_string_lossy();

    // Generate bulletin so channel context is complete
    let logger = spacebot::agent::cortex::CortexLogger::new(deps.sqlite_pool.clone());
    let bulletin_success = spacebot::agent::cortex::generate_bulletin(&deps, &logger).await;
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
        process_run_logger: spacebot::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone()),
        active_workers: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        worker_inputs: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        status_block: Arc::new(tokio::sync::RwLock::new(
            spacebot::agent::status::StatusBlock::new(),
        )),
        deps: deps.clone(),
        conversation_logger: conversation_logger.clone(),
        channel_store: channel_store.clone(),
        screenshot_dir: std::path::PathBuf::from("/tmp/screenshots"),
        logs_dir: std::path::PathBuf::from("/tmp/logs"),
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
    let branch_tool_server = spacebot::tools::create_branch_tool_server(
        deps.memory_search.clone(),
        conversation_logger,
        channel_store,
    );
    let branch_tool_defs = branch_tool_server.get_tool_defs(None).await.unwrap();
    let branch_tools_text = format_tool_defs(&branch_tool_defs);

    print_section("BRANCH SYSTEM PROMPT", &branch_prompt);
    print_stats("System prompt", &branch_prompt);
    print_section(
        &format!("BRANCH TOOLS ({} tools)", branch_tool_defs.len()),
        &branch_tools_text,
    );
    print_stats("Tool definitions", &branch_tools_text);
    let branch_total = branch_prompt.len() + branch_tools_text.len();
    println!("--- TOTAL BRANCH: ~{} tokens ---", branch_total / 4);

    // ── Worker ──
    let worker_prompt = prompt_engine
        .render_worker_prompt(&instance_dir, &workspace_dir)
        .expect("failed to render worker prompt");
    let browser_config = (**rc.browser_config.load()).clone();
    let brave_search_key = (**rc.brave_search_key.load()).clone();
    let worker_tool_server = spacebot::tools::create_worker_tool_server(
        deps.agent_id.clone(),
        uuid::Uuid::new_v4(),
        None,
        deps.event_tx.clone(),
        browser_config,
        std::path::PathBuf::from("/tmp/screenshots"),
        brave_search_key,
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/tmp"),
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
        "  USER.md:     {}",
        if identity.user.is_some() {
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
