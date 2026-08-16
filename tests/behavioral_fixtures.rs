//! Behavioral fixtures for the channel prompt rework (execution-plan 1.2).
//!
//! Harness: a pinned config in a temp instance dir → a real channel → a
//! canned conversation → one live LLM turn → assert the FIRST tool call
//! (name + key arguments) observed on the process event bus. N=5 samples per
//! fixture, 4/5 pass threshold. The fixtures encode the CURRENT prompt's
//! contract — delegation routing, silence, acknowledgment, memory handoff —
//! and must stay green through the 2.1 template rewrite.
//!
//! Live fixtures need `SPACEBOT_FIXTURE_API_KEY` (an Anthropic key — the
//! pinned config wires `anthropic_key`) and optionally
//! `SPACEBOT_FIXTURE_MODEL` (default `anthropic/claude-sonnet-4-20250514`).
//! They are `#[ignore]`d so a keyless environment reports them as ignored
//! rather than falsely green; run them deliberately with
//! `cargo test --test behavioral_fixtures -- --ignored --nocapture`.
//! The capability-consistency and byte-level suites are pure and always run.

use spacebot::agent::channel::{Channel, ChannelKind};
use spacebot::conversation::history::ConversationLogger;
use spacebot::conversation::settings::{DelegationMode, ResolvedConversationSettings};
use spacebot::{
    AgentDeps, ChannelId, InboundMessage, MessageContent, OutboundResponse, ProcessEvent,
    RoutedResponse,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::Duration;

const FIXTURE_AGENT_ID: &str = "fixture";
const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-4-20250514";
const SAMPLES: usize = 5;
const THRESHOLD: usize = 4;
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The pinned config template. `{model}` is substituted at write time.
const PINNED_CONFIG: &str = r#"
[llm]
anthropic_key = "env:SPACEBOT_FIXTURE_API_KEY"

[defaults.routing]
channel = "{model}"
branch = "{model}"
worker = "{model}"
compactor = "{model}"
cortex = "{model}"

[[agents]]
id = "fixture"
default = true
"#;

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

fn model() -> String {
    std::env::var("SPACEBOT_FIXTURE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

fn live_key_present() -> bool {
    std::env::var("SPACEBOT_FIXTURE_API_KEY").is_ok()
}

/// Write the pinned config into a fresh temp instance dir.
fn write_pinned_config(instance_dir: &Path) {
    let config = PINNED_CONFIG.replace("{model}", &model());
    std::fs::write(instance_dir.join("config.toml"), config)
        .expect("failed to write pinned fixture config");
    for dir in [
        "data",
        "agents/fixture/workspace",
        "agents/fixture/skills",
        "agents/fixture/data",
        "screenshots",
        "logs",
    ] {
        std::fs::create_dir_all(instance_dir.join(dir)).expect("failed to create fixture dir");
    }
}

/// Bootstrap AgentDeps from the pinned config in `instance_dir`.
async fn bootstrap(instance_dir: &Path) -> anyhow::Result<AgentDeps> {
    let config = spacebot::config::Config::load_from_path(&instance_dir.join("config.toml"))
        .expect("pinned config must parse");
    let resolved_agents = config.resolve_agents();
    let agent_config = resolved_agents.first().expect("fixture agent must resolve");

    let llm_manager = Arc::new(
        spacebot::llm::LlmManager::new(config.llm.clone())
            .await
            .expect("llm manager init"),
    );
    let embedding_model = Arc::new(
        spacebot::memory::EmbeddingModel::new(&instance_dir.join("data").join("embedding_cache"))
            .expect("embedding model init"),
    );
    let db = spacebot::db::Db::connect(&agent_config.data_dir)
        .await
        .expect("db connect");
    let instance_pool = spacebot::db::connect_instance_db(&instance_dir.join("data"))
        .await
        .expect("instance db connect");

    let memory_search = Arc::new(spacebot::memory::MemorySearch::new(
        spacebot::memory::MemoryStore::new(db.sqlite.clone()),
        spacebot::memory::EmbeddingTable::open_or_create(&db.lance)
            .await
            .expect("embedding table"),
        embedding_model,
    ));

    let identity = spacebot::identity::Identity::load(&agent_config.workspace).await;
    let prompts = spacebot::prompts::PromptEngine::new("en").expect("prompt engine");
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

    let buses = spacebot::create_process_event_buses_with_capacity(16, 32, 1024);
    let agent_id: spacebot::AgentId = Arc::from(FIXTURE_AGENT_ID);
    let task_store = Arc::new(spacebot::tasks::TaskStore::new(instance_pool.clone()));

    let sandbox_config = Arc::new(arc_swap::ArcSwap::from_pointee(
        agent_config.sandbox.clone(),
    ));
    let sandbox = Arc::new(
        spacebot::sandbox::Sandbox::new(
            sandbox_config,
            agent_config.workspace.clone(),
            &config.instance_dir,
            agent_config.data_dir.clone(),
            agent_id.clone(),
        )
        .await,
    );

    Ok(AgentDeps {
        agent_id,
        memory_search,
        llm_manager,
        mcp_manager: Arc::new(spacebot::mcp::McpManager::new(agent_config.mcp.clone())),
        task_store,
        goal_store: Arc::new(spacebot::goals::GoalStore::new(instance_pool.clone())),
        wake_event_store: Arc::new(spacebot::wakes::WakeEventStore::new(db.sqlite.clone())),
        autonomy_ceiling: Arc::new(arc_swap::ArcSwap::from_pointee(
            spacebot::config::AutonomyLevel::Act,
        )),
        wake_def_store: Arc::new(spacebot::wakes::WakeDefStore::new(db.sqlite.clone())),
        autonomy_run_store: Arc::new(spacebot::wakes::AutonomyRunStore::new(db.sqlite.clone())),
        autonomy_control: spacebot::agent::autonomy::AutonomyControl::default(),
        project_store: Arc::new(spacebot::projects::ProjectStore::new(instance_pool.clone())),
        cron_tool: None,
        runtime_config,
        event_tx: buses.control,
        memory_event_tx: buses.memory,
        tool_output_tx: buses.tool_output,
        sqlite_pool: db.sqlite.clone(),
        messaging_manager: None,
        sandbox,
        links: Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
        agent_names: Arc::new(HashMap::new()),
        humans: Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
        process_control_registry: Arc::new(
            spacebot::agent::process_control::ProcessControlRegistry::new(),
        ),
        injection_tx: tokio::sync::mpsc::channel(1).0,
        working_memory: spacebot::memory::WorkingMemoryStore::new(
            db.sqlite.clone(),
            chrono_tz::Tz::UTC,
        ),
        api_state: None,
        wiki_store: None,
        wake_tx: None,
    })
}

// ---------------------------------------------------------------------------
// Sample harness
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Sample {
    first_tool: Option<String>,
    tool_args: Option<String>,
    reply: Option<String>,
    timed_out: bool,
}

/// Run one live turn: seed the canned conversation, spawn a channel, send the
/// prompt, and observe the first tool call / reply.
async fn run_sample(
    deps: &AgentDeps,
    instance_dir: &Path,
    conversation: &[(&str, &str)],
    settings: ResolvedConversationSettings,
) -> Sample {
    let channel_id: ChannelId = Arc::from(format!("fixture:{}", uuid::Uuid::new_v4()));

    // Seed history so the turn has prior context.
    let logger = ConversationLogger::new(deps.sqlite_pool.clone());
    for (role, text) in conversation {
        match *role {
            "user" => {
                logger.log_user_message(&channel_id, "user", "user-1", text, &HashMap::new());
            }
            "assistant" => logger.log_bot_message(&channel_id, text),
            _ => {}
        }
    }
    logger.wait_for_pending_writes(Duration::from_secs(5)).await;

    let channel_event_rx = deps.event_tx.subscribe();
    let (response_tx, mut response_rx) = mpsc::channel::<RoutedResponse>(32);

    let (channel, channel_tx) = Channel::new(
        channel_id.clone(),
        ChannelKind::User,
        deps.clone(),
        response_tx,
        channel_event_rx,
        instance_dir.join("screenshots"),
        instance_dir.join("logs"),
        None,
        settings,
        None,
        None,
    );
    let channel_handle = tokio::spawn(async move { channel.run().await });

    let last = conversation.last().map(|(_, text)| *text).unwrap_or("");
    let message = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "fixture".into(),
        adapter: None,
        conversation_id: channel_id.to_string(),
        sender_id: "user-1".into(),
        agent_id: Some(deps.agent_id.clone()),
        content: MessageContent::Text(last.to_string()),
        timestamp: chrono::Utc::now(),
        metadata: HashMap::new(),
        formatted_author: None,
    };
    let _ = channel_tx.send(message).await;

    let mut sample = Sample::default();
    let mut events = deps.event_tx.subscribe();
    let deadline = tokio::time::sleep(SAMPLE_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(ProcessEvent::ToolStarted { tool_name, args, .. }) => {
                        sample.first_tool = Some(tool_name);
                        sample.tool_args = Some(args);
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            response = response_rx.recv() => {
                match response {
                    Some(RoutedResponse { response: OutboundResponse::Text(text), .. }) => {
                        sample.reply = Some(text);
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
            _ = &mut deadline => {
                sample.timed_out = true;
                break;
            }
        }
    }
    channel_handle.abort();
    sample
}

/// Run a fixture N times, report the sample table, assert the threshold.
async fn run_fixture(
    name: &str,
    conversation: &[(&str, &str)],
    settings: ResolvedConversationSettings,
    pass: impl Fn(&Sample) -> bool,
) {
    assert!(
        live_key_present(),
        "{name}: SPACEBOT_FIXTURE_API_KEY is not set. Live fixtures are \
         #[ignore]d for exactly this reason — run them deliberately with \
         `cargo test --test behavioral_fixtures -- --ignored`."
    );
    let instance = TempDir::new().expect("tempdir");
    write_pinned_config(instance.path());
    let deps = bootstrap(instance.path()).await.expect("bootstrap");

    let mut passes = 0;
    for sample_index in 0..SAMPLES {
        let sample = run_sample(&deps, instance.path(), conversation, settings.clone()).await;
        let ok = pass(&sample);
        passes += ok as usize;
        eprintln!(
            "  [{name}] sample {}/{}: {} {}{}",
            sample_index + 1,
            SAMPLES,
            if ok { "PASS" } else { "FAIL" },
            sample.first_tool.as_deref().unwrap_or("<no tool>"),
            sample
                .tool_args
                .as_deref()
                .map(|a| format!(" args={a}"))
                .unwrap_or_default()
        );
    }
    eprintln!("  [{name}] {passes}/{SAMPLES} passed (threshold {THRESHOLD}/{SAMPLES})");
    assert!(
        passes >= THRESHOLD,
        "{name}: {passes}/{SAMPLES} passed, below threshold {THRESHOLD}/{SAMPLES}"
    );
}

fn standard_settings() -> ResolvedConversationSettings {
    ResolvedConversationSettings::default()
}

fn direct_settings() -> ResolvedConversationSettings {
    ResolvedConversationSettings {
        delegation: DelegationMode::Direct,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Standard-mode fixtures
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn standard_mode_delegates_long_running_work() {
    run_fixture(
        "delegation_routing",
        &[(
            "user",
            "Write a Python script that parses my server logs and extracts error rates per hour",
        )],
        standard_settings(),
        |s| !s.timed_out && s.first_tool.as_deref() == Some("spawn_worker"),
    )
    .await;
}

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn standard_mode_stays_silent_on_trivial_input() {
    // Silence means the skip tool — a harness failure (timeout, no
    // observation) must not pass, and a text reply is not silence.
    run_fixture("silence", &[("user", "ok")], standard_settings(), |s| {
        !s.timed_out && s.first_tool.as_deref() == Some("skip")
    })
    .await;
}

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn standard_mode_acknowledges_without_delegating() {
    // A short reply or a skip both satisfy the policy; delegating or a
    // harness failure does not.
    run_fixture(
        "acknowledgment",
        &[("user", "Thanks for the help, that worked!")],
        standard_settings(),
        |s| {
            !s.timed_out
                && (matches!(s.first_tool.as_deref(), Some("reply" | "skip")) || s.reply.is_some())
        },
    )
    .await;
}

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn standard_mode_relays_worker_results() {
    run_fixture(
        "result_relay",
        &[(
            "user",
            "The docs worker just finished and reported the build passed. Let the user know.",
        )],
        standard_settings(),
        |s| !s.timed_out && (s.first_tool.as_deref() == Some("reply") || s.reply.is_some()),
    )
    .await;
}

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn standard_mode_hands_memory_intent_to_branch() {
    run_fixture(
        "memory_handoff",
        &[("user", "Remember that I only drink decaf coffee")],
        standard_settings(),
        |s| !s.timed_out && s.first_tool.as_deref() == Some("branch"),
    )
    .await;
}

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn standard_mode_answers_skill_question_without_delegating() {
    run_fixture(
        "skill_suggestion",
        &[(
            "user",
            "Is there a skill for writing conventional commit messages?",
        )],
        standard_settings(),
        |s| {
            !s.timed_out
                && (matches!(s.first_tool.as_deref(), Some("reply" | "skills_search"))
                    || s.reply.is_some())
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// Direct-mode fixtures
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn direct_mode_still_delegates_long_running_work() {
    run_fixture(
        "direct_delegation",
        &[(
            "user",
            "Summarize the last 200 commits across all our repos into a weekly report",
        )],
        direct_settings(),
        |s| !s.timed_out && s.first_tool.as_deref() == Some("spawn_worker"),
    )
    .await;
}

#[tokio::test]
#[ignore = "live fixture: requires SPACEBOT_FIXTURE_API_KEY"]
async fn direct_mode_still_branches_for_memory() {
    // Direct mode has memory tools registered, so saving inline is also a
    // correct outcome; losing the intent (no tool at all) is the failure.
    run_fixture(
        "direct_memory_handoff",
        &[("user", "Remember my home address is 123 Main Street")],
        direct_settings(),
        |s| !s.timed_out && matches!(s.first_tool.as_deref(), Some("branch" | "memory_save")),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Pure fixtures (no LLM)
// ---------------------------------------------------------------------------

/// Capability honesty, both directions. Positive: the routing-critical
/// tool names the prose owns (per-tool mechanics live in tool schemas since
/// 2.1) must appear for the modes that register them. Negative: prose gated
/// on a tool must vanish when that tool is not registered — this fixture's
/// deps carry no agent links, so `send_agent_message` must not be
/// advertised, and standard mode must not name the branch-side memory
/// tools it cannot call.
#[tokio::test]
async fn capability_consistency_prompt_advertises_registered_tools() {
    let instance = TempDir::new().expect("tempdir");
    write_pinned_config(instance.path());
    let deps = bootstrap(instance.path()).await.expect("bootstrap");

    let standard_tools = ["branch", "spawn_worker", "skip", "send_file"];
    let direct_tools = [
        "branch",
        "spawn_worker",
        "skip",
        "send_file",
        "memory_save",
        "memory_recall",
    ];
    let standard_absent = ["send_agent_message", "memory_save", "memory_recall"];
    let direct_absent = ["send_agent_message"];

    for (label, settings, tools, absent) in [
        (
            "standard",
            standard_settings(),
            &standard_tools[..],
            &standard_absent[..],
        ),
        (
            "direct",
            direct_settings(),
            &direct_tools[..],
            &direct_absent[..],
        ),
    ] {
        let channel_id: ChannelId = Arc::from(format!("cap:{label}"));
        let (response_tx, _) = mpsc::channel(4);
        let (channel, _channel_tx) = Channel::new(
            channel_id,
            ChannelKind::User,
            deps.clone(),
            response_tx,
            deps.event_tx.subscribe(),
            instance.path().join("screenshots"),
            instance.path().join("logs"),
            None,
            settings,
            None,
            None,
        );
        let prompt = channel.build_system_prompt().await.expect("prompt build");
        for tool in tools {
            assert!(
                prompt.contains(tool),
                "[{label}] prompt does not advertise registered tool `{tool}`"
            );
        }
        for tool in absent {
            assert!(
                !prompt.contains(tool),
                "[{label}] prompt advertises `{tool}`, which is not registered for this mode/config"
            );
        }
    }
}

/// The system prompt is byte-stable across turns on the same channel — the
/// 1.4 time-eviction guarantee. Wall-clock time rides the message envelope,
/// never the prompt.
#[tokio::test]
async fn byte_level_prompt_is_stable_across_turns() {
    let instance = TempDir::new().expect("tempdir");
    write_pinned_config(instance.path());
    let deps = bootstrap(instance.path()).await.expect("bootstrap");

    let channel_id: ChannelId = Arc::from("bytes:stable");
    let (response_tx, _) = mpsc::channel(4);
    let (channel, _channel_tx) = Channel::new(
        channel_id,
        ChannelKind::User,
        deps.clone(),
        response_tx,
        deps.event_tx.subscribe(),
        instance.path().join("screenshots"),
        instance.path().join("logs"),
        None,
        standard_settings(),
        None,
        None,
    );

    let first = channel.build_system_prompt().await.expect("first build");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = channel.build_system_prompt().await.expect("second build");

    assert_eq!(
        first, second,
        "system prompt must be byte-identical across turns"
    );
}

/// The block map must describe the prompt a real channel builds from the real
/// templates and identity files — not just the synthetic inputs the engine's
/// own tests use. If the blocks stop tiling, every byte range, token count and
/// proportion the inspector shows is describing something else.
#[tokio::test]
async fn block_map_tiles_a_real_channel_prompt() {
    let instance = TempDir::new().expect("tempdir");
    write_pinned_config(instance.path());
    let deps = bootstrap(instance.path()).await.expect("bootstrap");

    let channel_id: ChannelId = Arc::from("blocks:real");
    let (response_tx, _) = mpsc::channel(4);
    let (channel, _channel_tx) = Channel::new(
        channel_id,
        ChannelKind::User,
        deps.clone(),
        response_tx,
        deps.event_tx.subscribe(),
        instance.path().join("screenshots"),
        instance.path().join("logs"),
        None,
        standard_settings(),
        None,
        None,
    );

    let segmented = channel
        .build_system_prompt_segmented()
        .await
        .expect("segmented prompt build");

    assert!(
        !segmented.blocks.is_empty(),
        "a real channel prompt must produce a block map"
    );

    let mut cursor = 0usize;
    for block in &segmented.blocks {
        assert_eq!(
            block.start, cursor,
            "block `{}` leaves a gap or overlaps the previous one",
            block.id
        );
        assert!(
            segmented.text.is_char_boundary(block.start)
                && segmented.text.is_char_boundary(block.end),
            "block `{}` does not land on character boundaries",
            block.id
        );
        assert_eq!(
            segmented.text[block.start..block.end].chars().count(),
            block.chars,
            "block `{}` reports the wrong character count",
            block.id
        );
        cursor = block.end;
    }
    assert_eq!(
        cursor,
        segmented.text.len(),
        "blocks must account for every byte of the prompt"
    );

    // The plain accessor and the segmented one must agree, since the plain one
    // is what the byte-stability guarantee above is asserted against.
    let plain = channel.build_system_prompt().await.expect("plain build");
    assert_eq!(plain, segmented.text);

    // Sentinels are an implementation detail of segmentation and must never
    // survive into the bytes a provider receives.
    for sentinel in ['\u{E000}', '\u{E001}', '\u{E002}'] {
        assert!(
            !segmented.text.contains(sentinel),
            "block sentinel {sentinel:?} leaked into the rendered prompt"
        );
    }
}
