//! Cortex: System-level observer and memory bulletin generator.
//!
//! The cortex's primary responsibility is generating the **memory bulletin** — a
//! periodically refreshed, LLM-curated summary of the agent's current knowledge.
//! This bulletin is injected into every channel's system prompt, giving all
//! conversations ambient awareness of who the user is, what's been decided,
//! what happened recently, and what's going on.
//!
//! The cortex also observes system-wide activity via signals for future use in
//! health monitoring and memory consolidation.

use crate::error::Result;
use crate::llm::SpacebotModel;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort};
use crate::memory::types::MemoryType;
use crate::{AgentDeps, ProcessEvent, ProcessType};
use crate::hooks::CortexHook;

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// The cortex observes system-wide activity and maintains the memory bulletin.
pub struct Cortex {
    pub deps: AgentDeps,
    pub hook: CortexHook,
    /// Recent activity signals (rolling window).
    pub signal_buffer: Arc<RwLock<Vec<Signal>>>,
    /// System prompt loaded from prompts/CORTEX.md.
    pub system_prompt: String,
}

/// A high-level activity signal (not raw conversation).
#[derive(Debug, Clone)]
pub enum Signal {
    /// Channel started.
    ChannelStarted { channel_id: String },
    /// Channel ended.
    ChannelEnded { channel_id: String },
    /// Memory was saved.
    MemorySaved {
        memory_type: String,
        content_summary: String,
        importance: f32,
    },
    /// Worker completed.
    WorkerCompleted {
        task_summary: String,
        result_summary: String,
    },
    /// Compaction occurred.
    Compaction {
        channel_id: String,
        turns_compacted: i64,
    },
    /// Error occurred.
    Error {
        component: String,
        error_summary: String,
    },
}

impl Cortex {
    /// Create a new cortex.
    pub fn new(deps: AgentDeps, system_prompt: impl Into<String>) -> Self {
        let hook = CortexHook::new();

        Self {
            deps,
            hook,
            signal_buffer: Arc::new(RwLock::new(Vec::with_capacity(100))),
            system_prompt: system_prompt.into(),
        }
    }

    /// Process a process event and extract signals.
    pub async fn observe(&self, event: ProcessEvent) {
        let signal = match &event {
            ProcessEvent::MemorySaved { memory_id, .. } => Some(Signal::MemorySaved {
                memory_type: "unknown".into(),
                content_summary: format!("memory {}", memory_id),
                importance: 0.5,
            }),
            ProcessEvent::WorkerComplete { result, .. } => Some(Signal::WorkerCompleted {
                task_summary: "completed task".into(),
                result_summary: result.lines().next().unwrap_or("done").into(),
            }),
            ProcessEvent::CompactionTriggered {
                channel_id,
                threshold_reached,
                ..
            } => Some(Signal::Compaction {
                channel_id: channel_id.to_string(),
                turns_compacted: (*threshold_reached * 100.0) as i64,
            }),
            _ => None,
        };

        if let Some(signal) = signal {
            let mut buffer = self.signal_buffer.write().await;
            buffer.push(signal);

            if buffer.len() > 100 {
                buffer.remove(0);
            }

            tracing::debug!("cortex received signal, buffer size: {}", buffer.len());
        }
    }

    /// Run periodic consolidation (future: health monitoring, memory maintenance).
    pub async fn run_consolidation(&self) -> Result<()> {
        tracing::info!("cortex running consolidation");
        Ok(())
    }
}

/// Spawn the cortex bulletin loop for an agent.
///
/// Generates a memory bulletin immediately on startup, then refreshes on a
/// configurable interval. The bulletin is stored in `RuntimeConfig::memory_bulletin`
/// and injected into every channel's system prompt.
pub fn spawn_bulletin_loop(deps: AgentDeps) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_bulletin_loop(&deps).await {
            tracing::error!(%error, "cortex bulletin loop exited with error");
        }
    })
}

async fn run_bulletin_loop(deps: &AgentDeps) -> anyhow::Result<()> {
    tracing::info!("cortex bulletin loop started");

    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_SECS: u64 = 15;

    // Run immediately on startup, with retries
    for attempt in 0..=MAX_RETRIES {
        if generate_bulletin(deps).await {
            break;
        }
        if attempt < MAX_RETRIES {
            tracing::info!(
                attempt = attempt + 1,
                max = MAX_RETRIES,
                "retrying bulletin generation in {RETRY_DELAY_SECS}s"
            );
            tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
        }
    }

    loop {
        let cortex_config = **deps.runtime_config.cortex.load();
        let interval = cortex_config.bulletin_interval_secs;

        tokio::time::sleep(Duration::from_secs(interval)).await;

        generate_bulletin(deps).await;
    }
}

/// Bulletin sections: each defines a search mode + config, and how to label the
/// results when presenting them to the synthesis LLM.
struct BulletinSection {
    label: &'static str,
    mode: SearchMode,
    memory_type: Option<MemoryType>,
    sort_by: SearchSort,
    max_results: usize,
}

const BULLETIN_SECTIONS: &[BulletinSection] = &[
    BulletinSection {
        label: "Identity & Core Facts",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Identity),
        sort_by: SearchSort::Importance,
        max_results: 15,
    },
    BulletinSection {
        label: "Recent Memories",
        mode: SearchMode::Recent,
        memory_type: None,
        sort_by: SearchSort::Recent,
        max_results: 15,
    },
    BulletinSection {
        label: "Decisions",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Decision),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "High-Importance Context",
        mode: SearchMode::Important,
        memory_type: None,
        sort_by: SearchSort::Importance,
        max_results: 10,
    },
    BulletinSection {
        label: "Preferences & Patterns",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Preference),
        sort_by: SearchSort::Importance,
        max_results: 10,
    },
    BulletinSection {
        label: "Active Goals",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Goal),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "Recent Events",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Event),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "Observations",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Observation),
        sort_by: SearchSort::Recent,
        max_results: 5,
    },
];

/// Gather raw memory data for each bulletin section by querying the store directly.
/// Returns formatted sections ready for LLM synthesis.
async fn gather_bulletin_sections(deps: &AgentDeps) -> String {
    let mut output = String::new();

    for section in BULLETIN_SECTIONS {
        let config = SearchConfig {
            mode: section.mode,
            memory_type: section.memory_type,
            sort_by: section.sort_by,
            max_results: section.max_results,
            ..Default::default()
        };

        let results = match deps.memory_search.search("", &config).await {
            Ok(results) => results,
            Err(error) => {
                tracing::warn!(
                    section = section.label,
                    %error,
                    "bulletin section query failed"
                );
                continue;
            }
        };

        if results.is_empty() {
            continue;
        }

        output.push_str(&format!("### {}\n\n", section.label));
        for result in &results {
            output.push_str(&format!(
                "- [{}] (importance: {:.1}) {}\n",
                result.memory.memory_type,
                result.memory.importance,
                result.memory.content.lines().next().unwrap_or(&result.memory.content),
            ));
        }
        output.push('\n');
    }

    output
}

/// Generate a memory bulletin and store it in RuntimeConfig.
///
/// Programmatically queries the memory store across multiple dimensions
/// (identity, recent, decisions, importance, preferences, goals, events,
/// observations), then asks an LLM to synthesize the raw results into a
/// concise briefing.
///
/// On failure, the previous bulletin is preserved (not blanked out).
/// Returns `true` if the bulletin was successfully generated.
pub async fn generate_bulletin(deps: &AgentDeps) -> bool {
    tracing::info!("cortex generating memory bulletin");

    // Phase 1: Programmatically gather raw memory sections (no LLM needed)
    let raw_sections = gather_bulletin_sections(deps).await;

    if raw_sections.is_empty() {
        tracing::info!("no memories found, skipping bulletin synthesis");
        deps.runtime_config
            .memory_bulletin
            .store(Arc::new(String::new()));
        return true;
    }

    // Phase 2: LLM synthesis of raw sections into a cohesive bulletin
    let cortex_config = **deps.runtime_config.cortex.load();
    let bulletin_prompt = deps.runtime_config.prompts.load().cortex_bulletin.clone();

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model =
        SpacebotModel::make(&deps.llm_manager, &model_name).with_routing((**routing).clone());

    // No tools needed — the LLM just synthesizes the pre-gathered data
    let agent = AgentBuilder::new(model)
        .preamble(&bulletin_prompt)
        .build();

    let synthesis_prompt = format!(
        "Synthesize the following memory data into a concise briefing of {} words or fewer.\n\n\
         ## Raw Memory Data\n\n{raw_sections}",
        cortex_config.bulletin_max_words,
    );

    match agent.prompt(&synthesis_prompt).await {
        Ok(bulletin) => {
            let word_count = bulletin.split_whitespace().count();
            tracing::info!(
                words = word_count,
                bulletin = %bulletin,
                "cortex bulletin generated"
            );
            deps.runtime_config
                .memory_bulletin
                .store(Arc::new(bulletin));
            true
        }
        Err(error) => {
            tracing::error!(%error, "cortex bulletin synthesis failed, keeping previous bulletin");
            false
        }
    }
}


