use crate::error::Result;
use crate::prompts::blocks;
use anyhow::Context;
use minijinja::{Environment, Value, context};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// A completed background process result, passed to the retrigger template.
#[derive(Clone, Debug, Serialize)]
pub struct RetriggerResult {
    /// "branch" or "worker"
    pub process_type: String,
    /// The branch or worker ID (short UUID).
    pub process_id: String,
    /// Whether the process completed successfully.
    pub success: bool,
    /// The result/conclusion text from the process.
    pub result: String,
}

/// Named values for a template render.
///
/// Holds the inputs in one place so the plain render and the sentinel-marked
/// render are built from the same values rather than assembled twice.
#[derive(Debug, Default, Clone)]
pub struct PromptInputs {
    text: Vec<(&'static str, Option<String>)>,
    inline: Vec<(&'static str, Value)>,
}

impl PromptInputs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an optional value. `None` and `Some("")` are both absent, which is
    /// what the templates' `{%- if %}` guards already test for.
    pub fn text(mut self, name: &'static str, value: Option<String>) -> Self {
        self.text
            .push((name, value.filter(|value| !value.is_empty())));
        self
    }

    /// Add a value the template weaves into its own prose — a path, a flag,
    /// a list it iterates. These are never marked as blocks: a directory
    /// inside a sentence belongs to the sentence, and marking it would litter
    /// the map with fragments no reader is looking for.
    pub fn inline(mut self, name: &'static str, value: impl Serialize) -> Self {
        self.inline.push((name, Value::from_serialize(value)));
        self
    }

    /// The first input that would make the sentinel split ambiguous, if any.
    ///
    /// Inline values are checked too. They are never marked, but they still
    /// land in the rendered text, and a stray sentinel anywhere in the output
    /// misaligns every block after it.
    fn colliding_value(&self) -> Option<&'static str> {
        let marked = self.text.iter().find_map(|(name, value)| {
            value
                .as_deref()
                .is_some_and(blocks::collides_with_sentinels)
                .then_some(*name)
        });
        marked.or_else(|| {
            self.inline.iter().find_map(|(name, value)| {
                blocks::collides_with_sentinels(&value.to_string()).then_some(*name)
            })
        })
    }

    fn context(&self, instrument: bool) -> Value {
        let mut vars: HashMap<String, Value> = HashMap::new();
        for (name, value) in &self.text {
            let value = match value {
                Some(value) if instrument => Value::from(blocks::mark(name, value)),
                Some(value) => Value::from(value.clone()),
                None => Value::UNDEFINED,
            };
            vars.insert((*name).to_string(), value);
        }
        for (name, value) in &self.inline {
            vars.insert((*name).to_string(), value.clone());
        }
        Value::from_object(vars)
    }
}

/// Inputs to the channel system prompt, one field per template variable.
#[derive(Debug, Default, Clone)]
pub struct ChannelPromptInputs {
    pub identity_context: Option<String>,
    pub knowledge_synthesis: Option<String>,
    pub skills_prompt: Option<String>,
    pub worker_capabilities: String,
    pub conversation_context: Option<String>,
    pub status_text: Option<String>,
    pub available_channels: Option<String>,
    pub agent_links: bool,
    pub org_context: Option<String>,
    pub adapter_prompt: Option<String>,
    pub project_context: Option<String>,
    pub backfill_transcript: Option<String>,
    pub session_chronicle: Option<String>,
    pub working_memory: Option<String>,
    pub channel_activity_map: Option<String>,
    pub participant_context: Option<String>,
    pub active_goals: Option<String>,
    pub execution_mode: String,
    pub authority: String,
    pub autonomy_channel: bool,
}

impl ChannelPromptInputs {
    fn into_inputs(self) -> PromptInputs {
        PromptInputs::new()
            .text("identity_context", self.identity_context)
            .text("knowledge_synthesis", self.knowledge_synthesis)
            .text("skills_prompt", self.skills_prompt)
            .text("worker_capabilities", Some(self.worker_capabilities))
            .text("conversation_context", self.conversation_context)
            .text("status_text", self.status_text)
            .text("available_channels", self.available_channels)
            .text("org_context", self.org_context)
            .text("adapter_prompt", self.adapter_prompt)
            .text("project_context", self.project_context)
            .text("backfill_transcript", self.backfill_transcript)
            .text("session_chronicle", self.session_chronicle)
            .text("working_memory", self.working_memory)
            .text("channel_activity_map", self.channel_activity_map)
            .text("participant_context", self.participant_context)
            .text("active_goals", self.active_goals)
            .text("execution_mode", Some(self.execution_mode))
            .text("authority", Some(self.authority))
            .inline("autonomy_channel", self.autonomy_channel)
            .inline("agent_links", self.agent_links)
    }
}

/// Template engine for rendering system prompts with dynamic variables.
///
/// Prompts are bundled in the binary as `include_str!` embedded templates.
/// Language selection is done at initialization and templates are not
/// reloadable at runtime (no file watching, no hot reload).
#[derive(Clone)]
pub struct PromptEngine {
    /// The MiniJinja environment holding all templates for the configured language.
    /// Wrapped in Arc to make PromptEngine Clone.
    env: Arc<Environment<'static>>,
    /// Selected language code (e.g., "en").
    language: String,
}

impl PromptEngine {
    /// Create a new engine with templates for the given language.
    ///
    /// Currently only "en" (English) is fully implemented.
    /// The language parameter exists for future i18n expansion.
    pub fn new(language: &str) -> anyhow::Result<Self> {
        if language != "en" {
            tracing::warn!(
                language = language,
                "non-English language requested, falling back to English"
            );
        }

        let mut env = Environment::new();

        // Register all templates from the central text registry
        // Process prompts
        env.add_template("channel", crate::prompts::text::get("channel"))?;
        env.add_template(
            "autonomy_channel",
            crate::prompts::text::get("autonomy_channel"),
        )?;
        env.add_template("branch", crate::prompts::text::get("branch"))?;
        env.add_template("worker", crate::prompts::text::get("worker"))?;
        env.add_template("cortex", crate::prompts::text::get("cortex"))?;
        env.add_template(
            "cortex_intraday_synthesis",
            crate::prompts::text::get("cortex_intraday_synthesis"),
        )?;
        env.add_template(
            "cortex_daily_summary",
            crate::prompts::text::get("cortex_daily_summary"),
        )?;
        env.add_template("compactor", crate::prompts::text::get("compactor"))?;
        env.add_template(
            "chronicle_checkpoint",
            crate::prompts::text::get("chronicle_checkpoint"),
        )?;
        env.add_template(
            "chronicle_rollup",
            crate::prompts::text::get("chronicle_rollup"),
        )?;
        env.add_template(
            "memory_persistence",
            crate::prompts::text::get("memory_persistence"),
        )?;
        env.add_template("ingestion", crate::prompts::text::get("ingestion"))?;
        env.add_template("cortex_chat", crate::prompts::text::get("cortex_chat"))?;
        env.add_template(
            "cortex_profile",
            crate::prompts::text::get("cortex_profile"),
        )?;
        env.add_template("factory", crate::prompts::text::get("factory"))?;

        // Adapter-specific prompt fragments — every platform that
        // `render_channel_adapter_prompt` maps must be registered here.
        for adapter in [
            "adapters/email",
            "adapters/cron",
            "adapters/signal",
            "adapters/discord",
            "adapters/slack",
            "adapters/telegram",
            "adapters/mattermost",
            "adapters/portal",
            "adapters/twitch",
            "adapters/webhook",
        ] {
            env.add_template(adapter, crate::prompts::text::get(adapter))?;
        }

        // Slash-command agent-turn instructions
        env.add_template(
            "commands/tasks",
            crate::prompts::text::get("commands/tasks"),
        )?;
        env.add_template(
            "commands/today",
            crate::prompts::text::get("commands/today"),
        )?;
        env.add_template(
            "commands/digest",
            crate::prompts::text::get("commands/digest"),
        )?;

        // Fragment templates
        env.add_template(
            "fragments/worker_capabilities",
            crate::prompts::text::get("fragments/worker_capabilities"),
        )?;
        env.add_template(
            "fragments/conversation_context",
            crate::prompts::text::get("fragments/conversation_context"),
        )?;
        env.add_template(
            "fragments/skills_channel",
            crate::prompts::text::get("fragments/skills_channel"),
        )?;
        env.add_template(
            "fragments/skills_worker",
            crate::prompts::text::get("fragments/skills_worker"),
        )?;
        env.add_template(
            "fragments/skills_branch",
            crate::prompts::text::get("fragments/skills_branch"),
        )?;
        env.add_template(
            "fragments/available_channels",
            crate::prompts::text::get("fragments/available_channels"),
        )?;
        env.add_template(
            "fragments/execution_standard",
            crate::prompts::text::get("fragments/execution_standard"),
        )?;
        env.add_template(
            "fragments/execution_direct",
            crate::prompts::text::get("fragments/execution_direct"),
        )?;
        env.add_template(
            "fragments/authority",
            crate::prompts::text::get("fragments/authority"),
        )?;
        env.add_template(
            "fragments/org_context",
            crate::prompts::text::get("fragments/org_context"),
        )?;
        env.add_template(
            "fragments/projects_context",
            crate::prompts::text::get("fragments/projects_context"),
        )?;

        // System message fragments
        env.add_template(
            "fragments/system/retrigger",
            crate::prompts::text::get("fragments/system/retrigger"),
        )?;
        env.add_template(
            "fragments/system/retrigger_autonomy",
            crate::prompts::text::get("fragments/system/retrigger_autonomy"),
        )?;
        env.add_template(
            "fragments/system/truncation",
            crate::prompts::text::get("fragments/system/truncation"),
        )?;
        env.add_template(
            "fragments/system/worker_overflow",
            crate::prompts::text::get("fragments/system/worker_overflow"),
        )?;
        env.add_template(
            "fragments/system/worker_compact",
            crate::prompts::text::get("fragments/system/worker_compact"),
        )?;
        env.add_template(
            "fragments/system/memory_persistence",
            crate::prompts::text::get("fragments/system/memory_persistence"),
        )?;
        env.add_template(
            "fragments/system/memory_persistence_contract_retry",
            crate::prompts::text::get("fragments/system/memory_persistence_contract_retry"),
        )?;
        env.add_template(
            "fragments/system/autonomy_contract_retry",
            crate::prompts::text::get("fragments/system/autonomy_contract_retry"),
        )?;
        env.add_template(
            "fragments/system/profile_synthesis",
            crate::prompts::text::get("fragments/system/profile_synthesis"),
        )?;
        env.add_template(
            "fragments/system/ingestion_chunk",
            crate::prompts::text::get("fragments/system/ingestion_chunk"),
        )?;
        env.add_template(
            "fragments/system/history_backfill",
            crate::prompts::text::get("fragments/system/history_backfill"),
        )?;
        env.add_template(
            "fragments/system/tool_syntax_correction",
            crate::prompts::text::get("fragments/system/tool_syntax_correction"),
        )?;
        env.add_template(
            "fragments/tool_use_enforcement",
            crate::prompts::text::get("fragments/tool_use_enforcement"),
        )?;
        env.add_template(
            "fragments/coalesce_hint",
            crate::prompts::text::get("fragments/coalesce_hint"),
        )?;

        Ok(Self {
            env: Arc::new(env),
            language: language.to_string(),
        })
    }

    /// Render a template by name with the given context variables.
    ///
    /// # Arguments
    /// * `template_name` - Name of the template to render (e.g., "channel", "fragments/worker_capabilities")
    /// * `context` - MiniJinja Value containing template variables
    ///
    /// # Example
    /// ```rust,no_run
    /// use minijinja::context;
    /// # let engine = spacebot::prompts::engine::PromptEngine::new("en")?;
    /// let ctx = context! {
    ///     identity_context => "Some identity text",
    ///     browser_enabled => true,
    /// };
    /// let rendered = engine.render("channel", ctx)?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    /// Render a template from named inputs and map the result into blocks.
    ///
    /// The render is always instrumented: every injected value is wrapped in
    /// sentinels, and the returned text is that render with the sentinels
    /// stripped. There is deliberately no second, uninstrumented path — the
    /// bytes described by the block map are the only bytes this produces, so
    /// the map cannot describe a prompt that was never sent.
    ///
    /// The one exception is a value that already contains a sentinel
    /// character, which would make the split ambiguous. That renders plainly
    /// with an empty map rather than risk editing the value.
    pub fn render_segmented(
        &self,
        template_name: &str,
        inputs: PromptInputs,
    ) -> Result<blocks::SegmentedPrompt> {
        if let Some(colliding) = inputs.colliding_value() {
            tracing::warn!(
                template = template_name,
                input = colliding,
                "prompt input contains a block sentinel; rendering without a block map"
            );
            return Ok(blocks::SegmentedPrompt {
                text: self.render(template_name, inputs.context(false))?,
                blocks: Vec::new(),
            });
        }

        let instrumented = self.render(template_name, inputs.context(true))?;
        Ok(blocks::segment(&instrumented, template_name))
    }

    pub fn render(&self, template_name: &str, context: Value) -> Result<String> {
        let template = self
            .env
            .get_template(template_name)
            .with_context(|| format!("template '{}' not found", template_name))?;

        template
            .render(context)
            .with_context(|| format!("failed to render template '{}'", template_name))
            .map_err(Into::into)
    }

    /// Render a template with a HashMap of context variables.
    pub fn render_map(&self, template_name: &str, vars: HashMap<String, Value>) -> Result<String> {
        let context = Value::from_object(vars);
        self.render(template_name, context)
    }

    /// Convenience method for rendering simple templates with no variables.
    pub fn render_static(&self, template_name: &str) -> Result<String> {
        self.render(template_name, Value::UNDEFINED)
    }

    /// Render the tool-use enforcement fragment.
    pub fn render_tool_use_enforcement(&self) -> Result<String> {
        self.render_static("fragments/tool_use_enforcement")
    }

    /// Append tool-use enforcement guidance when configured for the model.
    pub fn maybe_append_tool_use_enforcement(
        &self,
        mut prompt: String,
        tool_use_enforcement: &crate::config::ToolUseEnforcement,
        model_name: &str,
    ) -> Result<String> {
        if !tool_use_enforcement.should_inject(model_name) {
            return Ok(prompt);
        }

        let guidance = self.render_tool_use_enforcement()?;
        let guidance = guidance.trim();
        if guidance.is_empty() {
            return Ok(prompt);
        }

        if !prompt.trim_end().is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(guidance);
        Ok(prompt)
    }

    /// Convenience method for rendering worker capabilities fragment.
    /// `worker_context` selects the context prose so the advertised behavior
    /// matches this conversation's actual worker settings.
    pub fn render_worker_capabilities(
        &self,
        browser_enabled: bool,
        web_search_enabled: bool,
        opencode_enabled: bool,
        mcp_tool_names: &[String],
        worker_context: &crate::conversation::settings::WorkerContextMode,
        project_manage_available: bool,
    ) -> Result<String> {
        self.render(
            "fragments/worker_capabilities",
            context! {
                browser_enabled => browser_enabled,
                web_search_enabled => web_search_enabled,
                opencode_enabled => opencode_enabled,
                mcp_tool_names => mcp_tool_names,
                worker_history_fork => matches!(
                    worker_context.history,
                    crate::conversation::settings::WorkerHistoryMode::Fork
                ),
                worker_memory => worker_context.memory.as_str(),
                project_manage_available => project_manage_available,
            },
        )
    }

    /// Convenience method for rendering conversation context fragment.
    pub fn render_conversation_context(
        &self,
        platform: &str,
        server_name: Option<&str>,
        channel_name: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<String> {
        self.render(
            "fragments/conversation_context",
            context! {
                platform => platform,
                server_name => server_name,
                channel_name => channel_name,
                conversation_id => conversation_id,
            },
        )
    }

    /// Convenience method for rendering skills channel fragment.
    pub fn render_skills_channel(
        &self,
        skills: Vec<SkillInfo>,
        category_descriptions: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        let categories = group_skills_by_category(skills, category_descriptions);
        self.render(
            "fragments/skills_channel",
            context! {
                categories => categories,
            },
        )
    }

    /// Render the memory persistence branch system prompt.
    ///
    /// `skill_reflection` adds the reflection section: the pass also decides
    /// whether the session produced a reusable procedure worth persisting as
    /// a skill.
    pub fn render_memory_persistence_prompt(
        &self,
        skill_reflection: bool,
        reflection_worker_ids: &[String],
    ) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented(
            "memory_persistence",
            PromptInputs::new()
                .inline("skill_reflection", skill_reflection)
                .inline("reflection_worker_ids", reflection_worker_ids),
        )
    }

    /// Render the skills listing for a branch system prompt.
    ///
    /// Branches read skills directly via `read_skill` or pass names to
    /// spawned workers as `suggested_skills`.
    pub fn render_skills_branch(
        &self,
        skills: Vec<SkillInfo>,
        category_descriptions: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        let categories = group_skills_by_category(skills, category_descriptions);
        self.render(
            "fragments/skills_branch",
            context! {
                categories => categories,
            },
        )
    }

    /// Render the worker system prompt with filesystem context and optional tool
    /// secret names.
    #[allow(clippy::too_many_arguments)]
    pub fn render_worker_prompt(
        &self,
        instance_dir: &str,
        workspace_dir: &str,
        sandbox_enabled: bool,
        sandbox_containment_active: bool,
        sandbox_read_allowlist: Vec<String>,
        sandbox_write_allowlist: Vec<String>,
        tool_secret_names: &[String],
        browser_persist_session: bool,
        status_text: Option<String>,
        wiki_enabled: bool,
        project_context: Option<String>,
    ) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented(
            "worker",
            PromptInputs::new()
                .text("status_text", status_text)
                .text("project_context", project_context)
                .inline("instance_dir", instance_dir)
                .inline("workspace_dir", workspace_dir)
                .inline("sandbox_enabled", sandbox_enabled)
                .inline("sandbox_containment_active", sandbox_containment_active)
                .inline("sandbox_read_allowlist", sandbox_read_allowlist)
                .inline("sandbox_write_allowlist", sandbox_write_allowlist)
                .inline("tool_secret_names", tool_secret_names)
                .inline("browser_persist_session", browser_persist_session)
                .inline("wiki_enabled", wiki_enabled),
        )
    }

    /// Render the branch system prompt with filesystem context.
    pub fn render_branch_prompt(
        &self,
        instance_dir: &str,
        workspace_dir: &str,
        wiki_enabled: bool,
    ) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented(
            "branch",
            PromptInputs::new()
                .inline("instance_dir", instance_dir)
                .inline("workspace_dir", workspace_dir)
                .inline("wiki_enabled", wiki_enabled),
        )
    }

    /// Render the available channels fragment for cross-channel awareness.
    pub fn render_available_channels(&self, channels: Vec<ChannelEntry>) -> Result<String> {
        self.render(
            "fragments/available_channels",
            context! {
                channels => channels,
            },
        )
    }

    /// Render the skills listing for a worker system prompt.
    ///
    /// Workers see all available skills with suggestions from the channel flagged.
    /// They read whichever skills they need via the read_skill tool.
    pub fn render_skills_worker(
        &self,
        skills: Vec<SkillInfo>,
        category_descriptions: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        let categories = group_skills_by_category(skills, category_descriptions);
        self.render(
            "fragments/skills_worker",
            context! {
                categories => categories,
            },
        )
    }

    /// Render the retrigger message with specific process results embedded.
    ///
    /// Each result includes the process type, ID, and full result text so the
    /// LLM knows exactly what completed and what to relay to the user.
    pub fn render_system_retrigger(&self, results: &[RetriggerResult]) -> Result<String> {
        self.render(
            "fragments/system/retrigger",
            context! {
                results => results,
            },
        )
    }

    /// Render the retrigger message for autonomy channels, which have no
    /// user-facing reply surface. Results are framed as run context to
    /// incorporate, not content that must be relayed to a user.
    pub fn render_system_retrigger_autonomy(&self, results: &[RetriggerResult]) -> Result<String> {
        self.render(
            "fragments/system/retrigger_autonomy",
            context! {
                results => results,
            },
        )
    }

    /// Correction message when the LLM outputs tool call syntax as plain text.
    pub fn render_system_tool_syntax_correction(&self) -> Result<String> {
        self.render_static("fragments/system/tool_syntax_correction")
    }

    /// Convenience method for rendering truncation marker.
    pub fn render_system_truncation(&self, remove_count: usize) -> Result<String> {
        self.render(
            "fragments/system/truncation",
            context! {
                remove_count => remove_count,
            },
        )
    }

    /// Convenience method for rendering worker overflow recovery message.
    pub fn render_system_worker_overflow(&self) -> Result<String> {
        self.render_static("fragments/system/worker_overflow")
    }

    /// Convenience method for rendering worker compaction message.
    pub fn render_system_worker_compact(&self, remove_count: usize, recap: &str) -> Result<String> {
        self.render(
            "fragments/system/worker_compact",
            context! {
                remove_count => remove_count,
                recap => recap,
            },
        )
    }

    /// Convenience method for rendering memory persistence prompt.
    pub fn render_system_memory_persistence(&self) -> Result<String> {
        self.render_static("fragments/system/memory_persistence")
    }

    /// Retry nudge sent to a memory-persistence branch that missed its terminal completion call.
    pub fn render_system_memory_persistence_contract_retry(&self) -> Result<String> {
        self.render_static("fragments/system/memory_persistence_contract_retry")
    }

    /// Retry nudge sent to an autonomy channel that missed its `autonomy_complete` call.
    pub fn render_system_autonomy_contract_retry(&self) -> Result<String> {
        self.render_static("fragments/system/autonomy_contract_retry")
    }

    /// Render the profile synthesis prompt with identity context.
    pub fn render_system_profile_synthesis(
        &self,
        identity_context: Option<&str>,
    ) -> Result<String> {
        self.render(
            "fragments/system/profile_synthesis",
            context! {
                identity_context => identity_context,
            },
        )
    }

    /// Render the intra-day synthesis prompt.
    pub fn render_intraday_synthesis(
        &self,
        event_count: usize,
        time_start: &str,
        time_end: &str,
        events: &str,
    ) -> Result<String> {
        self.render(
            "cortex_intraday_synthesis",
            context! {
                event_count => event_count,
                time_start => time_start,
                time_end => time_end,
                events => events,
            },
        )
    }

    /// Render the daily summary prompt.
    pub fn render_daily_summary(
        &self,
        date: &str,
        max_words: usize,
        intraday_blocks: &str,
    ) -> Result<String> {
        self.render(
            "cortex_daily_summary",
            context! {
                date => date,
                max_words => max_words,
                intraday_blocks => intraday_blocks,
            },
        )
    }

    /// Convenience method for rendering ingestion chunk prompt.
    pub fn render_system_ingestion_chunk(
        &self,
        filename: &str,
        chunk_number: usize,
        total_chunks: usize,
        chunk: &str,
    ) -> Result<String> {
        self.render(
            "fragments/system/ingestion_chunk",
            context! {
                filename => filename,
                chunk_number => chunk_number,
                total_chunks => total_chunks,
                chunk => chunk,
            },
        )
    }

    /// Render the history backfill wrapper with instructions not to act on it.
    pub fn render_system_history_backfill(&self, transcript: &str) -> Result<String> {
        self.render(
            "fragments/system/history_backfill",
            context! {
                transcript => transcript,
            },
        )
    }

    /// Render the coalesce hint fragment for batched messages.
    pub fn render_coalesce_hint(
        &self,
        message_count: usize,
        elapsed: &str,
        unique_senders: usize,
    ) -> Result<String> {
        self.render(
            "fragments/coalesce_hint",
            context! {
                message_count => message_count,
                elapsed => elapsed,
                unique_senders => unique_senders,
            },
        )
    }

    /// Render the autonomy channel run briefing.
    ///
    /// This becomes the run's initial synthetic message; the channel's normal
    /// system prompt (identity, memory store, working memory) is layered on top
    /// by the channel machinery.
    #[allow(clippy::too_many_arguments)]
    pub fn render_autonomy_channel_prompt(
        &self,
        agent_name: &str,
        level: &str,
        wake_events: Vec<AutonomyWakeEventView>,
        run_history: Vec<AutonomyRunHistoryView>,
        task_state: &str,
        active_goals: Option<&str>,
        active_workers: Option<&str>,
        max_tasks_per_run: u32,
        claim_unowned: bool,
        instance_is_empty: bool,
        trigger_reason: &str,
        elapsed_secs: Option<u64>,
    ) -> Result<String> {
        self.render(
            "autonomy_channel",
            context! {
                agent_name => agent_name,
                level => level,
                wake_events => wake_events,
                run_history => run_history,
                task_state => task_state,
                active_goals => active_goals,
                active_workers => active_workers,
                max_tasks_per_run => max_tasks_per_run,
                claim_unowned => claim_unowned,
                instance_is_empty => instance_is_empty,
                trigger_reason => trigger_reason,
                elapsed_secs => elapsed_secs,
            },
        )
    }

    /// Render optional adapter-specific channel guidance.
    ///
    /// Only adapters whose formatting claims passed converter verification
    /// (1.5) ship a rendering fragment; the rest stay silent until their
    /// converter is fixed. cron/email/signal fragments are channel
    /// semantics, not rendering — they stay as they are.
    pub fn render_channel_adapter_prompt(&self, adapter: &str) -> Option<String> {
        let template_name = match adapter {
            "email" => "adapters/email",
            "cron" => "adapters/cron",
            "signal" => "adapters/signal",
            "discord" => "adapters/discord",
            "slack" => "adapters/slack",
            "telegram" => "adapters/telegram",
            "mattermost" => "adapters/mattermost",
            "portal" => "adapters/portal",
            "twitch" => "adapters/twitch",
            "webhook" => "adapters/webhook",
            _ => return None,
        };

        match self.render_static(template_name) {
            Ok(value) => {
                let value = value.trim().to_string();
                if value.is_empty() { None } else { Some(value) }
            }
            Err(error) => {
                tracing::error!(template_name, %error, "failed to render adapter prompt template");
                None
            }
        }
    }

    /// Render the cortex chat system prompt with optional channel context.
    #[allow(clippy::too_many_arguments)]
    pub fn render_cortex_chat_prompt(
        &self,
        identity_context: Option<String>,
        channel_transcript: Option<String>,
        agents_manifest: Option<String>,
        changelog_highlights: Option<String>,
        runtime_config_snapshot: Option<String>,
        worker_capabilities: String,
        factory_enabled: bool,
    ) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented(
            "cortex_chat",
            PromptInputs::new()
                .text("identity_context", identity_context)
                .text("channel_transcript", channel_transcript)
                .text("agents_manifest", agents_manifest)
                .text("changelog_highlights", changelog_highlights)
                .text("runtime_config_snapshot", runtime_config_snapshot)
                .text("worker_capabilities", Some(worker_capabilities))
                .inline("factory_enabled", factory_enabled),
        )
    }

    /// Render the factory system prompt for agent creation conversations.
    ///
    /// The factory prompt instructs the LLM on how to create and configure new agents
    /// using preset archetypes, organizational memory, and user preferences.
    pub fn render_factory_prompt(
        &self,
        identity_context: Option<String>,
    ) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented(
            "factory",
            PromptInputs::new().text("identity_context", identity_context),
        )
    }

    /// Render a prompt that takes no variables, mapped as one template block.
    ///
    /// The map is a single entry, which is the honest description: nothing was
    /// injected. It still lets the inspector report the prompt as mapped rather
    /// than as an unmapped wall of text.
    pub fn render_static_segmented(&self, template_name: &str) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented(template_name, PromptInputs::new())
    }

    /// Render the org context fragment showing the agent's position in the hierarchy.
    ///
    /// `human_profile_cap` caps each HUMAN.md profile: under 2x the cap the
    /// profile renders in full (the block header reports utilization against
    /// the pre-cap total, loudly past 100%); past 2x it truncates at a
    /// section boundary and the header discloses the truncation.
    pub fn render_org_context(
        &self,
        mut org_context: OrgContext,
        human_profile_cap: usize,
    ) -> Result<String> {
        // A zero cap would divide by zero in the fragment's header math.
        let human_profile_cap = human_profile_cap.max(1);
        for group in [
            &mut org_context.superiors,
            &mut org_context.subordinates,
            &mut org_context.peers,
        ] {
            for entry in group {
                if let Some(description) = entry.description.take() {
                    entry.description_total_chars = Some(description.chars().count());
                    entry.description = Some(cap_human_description(description, human_profile_cap));
                }
            }
        }
        self.render(
            "fragments/org_context",
            context! {
                org_context => org_context,
                human_profile_cap => human_profile_cap,
            },
        )
    }

    /// Render the projects context fragment listing active projects with repos and worktrees.
    ///
    /// Bounded to `MAX_PROJECTS` projects, unreported (2.2).
    pub fn render_projects_context(&self, projects: Vec<ProjectContext>) -> Result<String> {
        const MAX_PROJECTS: usize = 10;
        let projects: Vec<ProjectContext> = projects.into_iter().take(MAX_PROJECTS).collect();
        self.render(
            "fragments/projects_context",
            context! {
                projects => projects,
            },
        )
    }

    /// Render the channel system prompt with all dynamic components including org context.
    #[allow(clippy::too_many_arguments)]
    /// Render the channel system prompt along with its block map.
    pub fn render_channel_prompt(
        &self,
        inputs: ChannelPromptInputs,
    ) -> Result<blocks::SegmentedPrompt> {
        self.render_segmented("channel", inputs.into_inputs())
    }

    /// Get the configured language code.
    pub fn language(&self) -> &str {
        &self.language
    }
}

/// Cap a human profile at a section boundary past 2x the budget. Under 2x the
/// cap it renders in full — the utilization header reports >100% loudly, per
/// the 2.2 design; past 2x it truncates at the last section heading within
/// the ceiling so operator-authored standing rules in the tail are never
/// silently cut mid-section.
/// Cap an authored profile document for rendering.
///
/// Under 2x the cap the document renders in full — the header's utilization
/// number is the pressure signal, and authored documents may carry standing
/// rules in their tail, so silent truncation is never acceptable under the
/// ceiling. Past the ceiling the cut lands only at a content boundary: the
/// last markdown heading outside a code fence within the ceiling, or the
/// last blank line if no heading exists. A document with neither renders in
/// full — an oversized render is better than a mid-sentence cut.
fn cap_human_description(description: String, cap: usize) -> String {
    let ceiling = cap.saturating_mul(2);
    if description.chars().count() <= ceiling {
        return description;
    }
    let prefix: String = description.chars().take(ceiling).collect();

    let mut in_fence = false;
    let mut last_heading: Option<usize> = None;
    let mut last_blank: Option<usize> = None;
    let mut offset = 0;
    for line in prefix.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence {
            if trimmed.starts_with('#') && offset > 0 {
                last_heading = Some(offset);
            } else if line.trim().is_empty() {
                last_blank = Some(offset);
            }
        }
        offset += line.len();
    }

    match last_heading.or(last_blank) {
        Some(idx) if idx > 0 => format!("{}…\n", prefix[..idx].trim_end()),
        _ => description,
    }
}

/// Organizational context for an agent — grouped by relationship.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrgContext {
    pub superiors: Vec<LinkedAgent>,
    pub subordinates: Vec<LinkedAgent>,
    pub peers: Vec<LinkedAgent>,
}

/// Information about a linked agent or human for prompt rendering.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkedAgent {
    pub name: String,
    pub id: String,
    /// Whether this is a human (true) or an agent (false).
    pub is_human: bool,
    /// The human's role (e.g. "Founder", "Lead Developer"). Only set for humans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Rich context about the human — background, preferences, communication
    /// style, etc. Loaded from `HUMAN.md` on disk. Only set for humans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Character count of the profile before capping, so the block header
    /// reports true utilization even when the render is truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_total_chars: Option<usize>,
}

/// A pending wake event rendered into the autonomy run briefing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutonomyWakeEventView {
    pub wake_id: String,
    /// Wake definition name; falls back to the wake id when the definition
    /// no longer exists.
    pub name: String,
    /// Wake instructions, included only when the wake's min_level is within
    /// the current autonomy level.
    pub instructions: Option<String>,
    /// The wake exists but its min_level is above the current level: the
    /// event is surfaced as an observation only.
    pub gated: bool,
    pub fired_at: String,
    pub delivery_count: i64,
    /// Compact JSON payload preview; empty when the payload is empty.
    pub payload: String,
}

/// A past autonomy run rendered into the run briefing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutonomyRunHistoryView {
    pub started_at: String,
    pub status: String,
    pub summary: String,
    /// How many wake events that run consumed.
    pub woken_by: usize,
}

/// Information about a skill for template rendering.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub location: String,
    /// Whether the spawning channel suggested this skill for the current task.
    /// Workers should prioritise suggested skills but may read others too.
    pub suggested: bool,
    /// Category derived from the directory path.
    pub category: String,
}

/// Group of skills under one category for grouped index rendering.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillCategoryGroup {
    pub name: String,
    /// Description from the category's `index.md`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub skills: Vec<SkillInfo>,
}

/// Group skills by category, sorted by category name then skill name within
/// each group. Category descriptions come from `index.md` files loaded
/// during discovery.
fn group_skills_by_category(
    skills: Vec<SkillInfo>,
    category_descriptions: &std::collections::HashMap<String, String>,
) -> Vec<SkillCategoryGroup> {
    let mut groups: std::collections::BTreeMap<String, Vec<SkillInfo>> =
        std::collections::BTreeMap::new();
    for skill in skills {
        groups
            .entry(skill.category.clone())
            .or_default()
            .push(skill);
    }
    groups
        .into_iter()
        .map(|(name, mut skills)| {
            skills.sort_by(|a, b| a.name.cmp(&b.name));
            let description = category_descriptions.get(&name).cloned();
            SkillCategoryGroup {
                name,
                description,
                skills,
            }
        })
        .collect()
}

/// Information about a channel for template rendering.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelEntry {
    pub name: String,
    pub platform: String,
    pub id: String,
}

/// A project's context for prompt injection — repos and active worktrees.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectContext {
    pub name: String,
    pub root_path: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub repos: Vec<ProjectRepoContext>,
    pub worktrees: Vec<ProjectWorktreeContext>,
}

/// A repo within a project for prompt injection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectRepoContext {
    pub name: String,
    pub path: String,
    pub default_branch: String,
    pub remote_url: Option<String>,
}

/// A worktree within a project for prompt injection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectWorktreeContext {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub repo_name: String,
}

// All templates are now loaded from the centralized text registry (src/prompts/text.rs)

#[cfg(test)]
mod tests {
    use super::{ChannelPromptInputs, PromptEngine};
    use crate::config::ToolUseEnforcement;

    /// A channel prompt with only the always-present fragments filled in.
    fn base_inputs(engine: &PromptEngine) -> ChannelPromptInputs {
        ChannelPromptInputs {
            execution_mode: engine
                .render_static("fragments/execution_standard")
                .unwrap_or_default(),
            authority: engine
                .render_static("fragments/authority")
                .unwrap_or_default(),
            ..ChannelPromptInputs::default()
        }
    }

    /// The block map must describe the prompt that would have been sent
    /// without instrumentation. If this ever fails, every byte range and token
    /// count in the inspector is describing a prompt the model never saw.
    #[test]
    fn segmented_render_matches_plain_render_byte_for_byte() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");

        let filled = || ChannelPromptInputs {
            identity_context: Some("# Orion\n\nI work for Jamie.".to_string()),
            knowledge_synthesis: Some("## Memory Store\n\nScope: global".to_string()),
            skills_prompt: Some("## Available Skills\n\n- deploy".to_string()),
            worker_capabilities: "## Worker Types\n\nBuiltin only.".to_string(),
            conversation_context: Some("Platform: telegram".to_string()),
            status_text: Some("No active processes.".to_string()),
            available_channels: Some("## Available Channels\n\n- ops".to_string()),
            agent_links: true,
            org_context: Some("## Organization\n\nFlat.".to_string()),
            adapter_prompt: Some("Keep replies short.".to_string()),
            project_context: Some("## Active Projects\n\n- spacebot".to_string()),
            backfill_transcript: Some("[]".to_string()),
            session_chronicle: Some("## Chronicle\n\nEarlier today.".to_string()),
            working_memory: Some("## Working Memory\n\nNothing yet.".to_string()),
            channel_activity_map: Some("## Channel Activity\n\nQuiet.".to_string()),
            participant_context: Some("## Participants\n\nJamie.".to_string()),
            active_goals: Some("## Goals\n\nShip it.".to_string()),
            ..base_inputs(&engine)
        };

        let plain = engine
            .render("channel", filled().into_inputs().context(false))
            .expect("plain render");
        let segmented = engine
            .render_channel_prompt(filled())
            .expect("segmented render");

        assert_eq!(
            segmented.text, plain,
            "stripping the sentinels must reproduce the uninstrumented render"
        );

        // Every mapped range must address exactly the bytes it claims.
        for block in &segmented.blocks {
            assert!(
                block.end <= segmented.text.len(),
                "block {} runs past the prompt",
                block.id
            );
            assert!(segmented.text.is_char_boundary(block.start));
            assert!(segmented.text.is_char_boundary(block.end));
        }

        // Blocks must tile the prompt in order without gaps or overlaps, so a
        // reader scrolling the raw text is never inside an unlabelled region.
        let mut cursor = 0usize;
        for block in &segmented.blocks {
            assert_eq!(
                block.start, cursor,
                "gap or overlap before block {}",
                block.id
            );
            cursor = block.end;
        }
        assert_eq!(cursor, segmented.text.len(), "blocks must reach the end");
    }

    /// Worker prompts weave paths, flags and allowlists into their own
    /// sentences. Those are inline inputs, not sections — marking them would
    /// litter the map with fragments — so the map should hold the template's
    /// prose plus the two real sections.
    #[test]
    fn worker_prompt_maps_sections_and_inlines_scalars() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");

        let segmented = engine
            .render_worker_prompt(
                "/instance",
                "/workspace",
                true,
                true,
                vec!["/workspace".to_string()],
                vec!["/workspace/out".to_string()],
                &["OPENAI_API_KEY".to_string()],
                false,
                Some("No active processes.".to_string()),
                false,
                Some("## Active Projects\n\n- spacebot".to_string()),
            )
            .expect("worker prompt should render");

        let ids: Vec<&str> = segmented
            .blocks
            .iter()
            .map(|block| block.id.as_str())
            .collect();
        assert!(
            ids.contains(&"status_text"),
            "sections must be mapped: {ids:?}"
        );
        assert!(
            ids.contains(&"project_context"),
            "sections must be mapped: {ids:?}"
        );
        assert!(
            !ids.iter().any(|id| id.contains("workspace_dir")),
            "a path woven into a sentence is prose, not a block: {ids:?}"
        );

        // The scalars still reach the template.
        assert!(segmented.text.contains("/workspace"));
        assert!(segmented.text.contains("/workspace/out"));

        assert_tiles(&segmented);
    }

    /// A prompt with no injected values is one block covering all of it —
    /// which is what the inspector should say, rather than reporting it as
    /// unmapped.
    #[test]
    fn static_prompt_maps_to_a_single_template_block() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let segmented = engine
            .render_static_segmented("compactor")
            .expect("compactor prompt should render");

        assert_eq!(segmented.blocks.len(), 1);
        assert_eq!(segmented.blocks[0].id, "template:compactor");
        assert_eq!(
            segmented.text,
            engine.render_static("compactor").expect("plain render"),
        );
        assert_tiles(&segmented);
    }

    #[test]
    fn appended_sections_extend_the_map() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let mut segmented = engine
            .render_branch_prompt("/instance", "/workspace", false)
            .expect("branch prompt should render");

        let before = segmented.blocks.len();
        segmented.append_section("skills_prompt", "## Available Skills\n\n- deploy");
        segmented.append_section("tool_use_enforcement", "Always call a tool.");
        // An empty section is skipped rather than recorded as a zero-width block.
        segmented.append_section("required_skills", "");

        assert_eq!(segmented.blocks.len(), before + 2);
        assert!(segmented.text.ends_with("Always call a tool."));
        assert_tiles(&segmented);
    }

    /// Blocks must account for every byte, in order, with no gaps or overlaps.
    fn assert_tiles(segmented: &crate::prompts::SegmentedPrompt) {
        let mut cursor = 0usize;
        for block in &segmented.blocks {
            assert_eq!(block.start, cursor, "gap or overlap at block {}", block.id);
            assert_eq!(
                segmented.text[block.start..block.end].chars().count(),
                block.chars,
                "block {} reports the wrong character count",
                block.id
            );
            cursor = block.end;
        }
        assert_eq!(cursor, segmented.text.len(), "blocks must reach the end");
    }

    #[test]
    fn absent_inputs_produce_no_blocks() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let segmented = engine
            .render_channel_prompt(ChannelPromptInputs {
                worker_capabilities: "## Worker Types\n\nBuiltin only.".to_string(),
                ..base_inputs(&engine)
            })
            .expect("segmented render");

        assert!(
            !segmented
                .blocks
                .iter()
                .any(|block| block.id == "status_text"),
            "an absent input must not appear in the block map"
        );
        assert!(
            segmented
                .blocks
                .iter()
                .any(|block| block.id == "worker_capabilities"),
            "a present input must appear in the block map"
        );
    }

    #[test]
    fn cap_human_description_renders_small_docs_in_full() {
        let doc = "# Profile\n\nShort.".to_string();
        assert_eq!(super::cap_human_description(doc.clone(), 4_000), doc);
    }

    #[test]
    fn every_mapped_adapter_template_is_registered() {
        let engine = PromptEngine::new("en").expect("engine builds");
        for adapter in [
            "email",
            "cron",
            "signal",
            "discord",
            "slack",
            "telegram",
            "mattermost",
            "portal",
            "twitch",
            "webhook",
        ] {
            // A mapped adapter whose template is missing renders as None and
            // logs an error; assert the render path finds the template.
            assert!(
                engine.render_channel_adapter_prompt(adapter).is_some(),
                "adapter fragment for {adapter} failed to render"
            );
        }
    }

    #[test]
    fn cap_human_description_over_cap_under_ceiling_is_untouched() {
        // 1.5x the cap: over budget but under the 2x ceiling — the header
        // carries the pressure signal, the content stays whole.
        let doc = format!("# A\n\n{}", "x".repeat(150));
        assert_eq!(super::cap_human_description(doc.clone(), 100), doc);
    }

    #[test]
    fn cap_human_description_cuts_at_section_boundary_past_ceiling() {
        let body = "y".repeat(120);
        let doc = format!("# Head\n\n{body}\n\n# Tail\n\n{}", "z".repeat(200));
        let capped = super::cap_human_description(doc, 100);
        assert!(capped.contains(&body));
        assert!(!capped.contains("# Tail"));
        assert!(capped.ends_with("…\n"));
    }

    #[test]
    fn cap_human_description_ignores_headings_inside_code_fences() {
        let filler = "w".repeat(150);
        let doc = format!(
            "# Head\n\n```\n# not a heading\n```\n{filler}\n{}",
            "v".repeat(200)
        );
        let capped = super::cap_human_description(doc, 100);
        // The fenced pseudo-heading is not a cut point; the cut falls back
        // to a blank-line boundary instead.
        assert!(!capped.ends_with("# not a heading…\n"));
    }

    #[test]
    fn cap_human_description_without_boundaries_renders_in_full() {
        let doc = "q".repeat(500);
        assert_eq!(super::cap_human_description(doc.clone(), 100), doc);
    }

    #[test]
    fn appends_tool_use_enforcement_for_matching_model() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let prompt = engine
            .maybe_append_tool_use_enforcement(
                "Base prompt".to_string(),
                &ToolUseEnforcement::Auto,
                "openai/gpt-4.1",
            )
            .expect("tool-use guidance should render");

        assert!(prompt.contains("Base prompt"));
        assert!(prompt.contains("Tool-Use Enforcement"));
    }

    #[test]
    fn skips_tool_use_enforcement_for_non_matching_model() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let prompt = engine
            .maybe_append_tool_use_enforcement(
                "Base prompt".to_string(),
                &ToolUseEnforcement::Auto,
                "anthropic/claude-sonnet-4",
            )
            .expect("tool-use guidance should render");

        assert_eq!(prompt, "Base prompt");
    }

    #[test]
    fn renders_memory_store_and_gates_linked_agents() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let render = |agent_links: bool| {
            engine
                .render_channel_prompt(ChannelPromptInputs {
                    knowledge_synthesis: Some("## Memory Store\n\nScope: global".to_string()),
                    agent_links,
                    ..base_inputs(&engine)
                })
                .expect("channel prompt should render")
                .text
        };

        let unlinked = render(false);
        assert!(unlinked.contains("## Memory Store"));
        assert!(
            !unlinked.contains("send_agent_message"),
            "linked-agent guidance must not render when no link tool is registered"
        );

        let linked = render(true);
        assert!(linked.contains("## Linked Agents"));
        assert!(linked.contains("send_agent_message"));
    }

    #[test]
    fn renders_session_chronicle_block_when_supplied() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let render = |chronicle: Option<String>| {
            engine
                .render_channel_prompt(ChannelPromptInputs {
                    session_chronicle: chronicle,
                    ..base_inputs(&engine)
                })
                .expect("channel prompt should render")
                .text
        };

        let without = render(None);
        assert!(!without.contains("Session Chronicle"));

        let with = render(Some("## Session Chronicle\n\nTwo checkpoints.".to_string()));
        assert!(with.contains("## Session Chronicle"));
        assert!(with.contains("Two checkpoints."));
        assert!(
            with.contains("history, not instruction"),
            "the chronicle block carries its own read-only framing"
        );
    }

    #[test]
    fn memory_persistence_prompt_gates_reflection_section() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");

        let plain = engine
            .render_memory_persistence_prompt(false, &[])
            .expect("persistence prompt should render")
            .text;
        assert!(plain.contains("memory persistence process"));
        assert!(!plain.contains("## Skill Reflection"));

        let reflecting = engine
            .render_memory_persistence_prompt(true, &[])
            .expect("reflection prompt should render")
            .text;
        assert!(reflecting.contains("## Skill Reflection"));
        assert!(reflecting.contains("never the incident"));
        assert!(reflecting.contains("Never persist"));
        assert!(!reflecting.contains("completed since the last reflection pass"));

        let worker_ids = vec!["92ae6824-dd29-4f10-bdbe-8e33b4faa35d".to_string()];
        let with_workers = engine
            .render_memory_persistence_prompt(true, &worker_ids)
            .expect("reflection prompt with workers should render")
            .text;
        assert!(with_workers.contains("92ae6824-dd29-4f10-bdbe-8e33b4faa35d"));
        assert!(with_workers.contains("worker_inspect"));
    }

    #[test]
    fn autonomy_channel_prompt_renders_per_level() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");

        let wake_events = vec![
            super::AutonomyWakeEventView {
                wake_id: "ci-failed".to_string(),
                name: "CI failed on main".to_string(),
                instructions: Some("Investigate the failing job.".to_string()),
                gated: false,
                fired_at: "2026-08-09T02:00:00Z".to_string(),
                delivery_count: 3,
                payload: "{\"job\":\"clippy\"}".to_string(),
            },
            super::AutonomyWakeEventView {
                wake_id: "task-approved".to_string(),
                name: "Task approved".to_string(),
                instructions: None,
                gated: true,
                fired_at: "2026-08-09T02:05:00Z".to_string(),
                delivery_count: 1,
                payload: String::new(),
            },
        ];
        let run_history = vec![super::AutonomyRunHistoryView {
            started_at: "2026-08-09T00:00:00Z".to_string(),
            status: "completed".to_string(),
            summary: "Enriched task #4.".to_string(),
            woken_by: 1,
        }];

        let observe = engine
            .render_autonomy_channel_prompt(
                "Iris",
                "observe",
                wake_events.clone(),
                run_history.clone(),
                "### Pending approval\n- #4 [high] Investigate flaky test\n",
                Some("### [HIGH] Ship v2"),
                None,
                2,
                true,
                false,
                "heartbeat",
                Some(480),
            )
            .expect("observe prompt should render");
        assert!(observe.contains("You are Iris."));
        assert!(observe.contains("CI failed on main"));
        assert!(observe.contains("Instructions: Investigate the failing job."));
        assert!(observe.contains("observe only; this event requires a higher autonomy level"));
        assert!(observe.contains("3 coalesced firings"));
        assert!(observe.contains("Enriched task #4."));
        assert!(observe.contains("Your autonomy level is **observe**"));
        assert!(observe.contains("at most 2 tasks in this epoch"));
        assert!(observe.contains("active for 480 seconds"));
        assert!(observe.contains("Never execute `pending_approval` tasks"));

        let act = engine
            .render_autonomy_channel_prompt(
                "Iris",
                "act",
                Vec::new(),
                Vec::new(),
                "No active tasks.\n",
                None,
                None,
                1,
                false,
                true,
                "heartbeat",
                None,
            )
            .expect("act prompt should render");
        assert!(act.contains("No new wake events"));
        assert!(act.contains("execute `ready` tasks"));
        assert!(act.contains("at most 1 task in this epoch"));
        assert!(!act.contains("claim unowned work"));
        assert!(act.contains("The board and goal list are empty"));
        assert!(act.contains("Never invent tasks to look busy."));

        // An unrecognized level falls back to observe-only rules.
        let unknown = engine
            .render_autonomy_channel_prompt(
                "Iris",
                "unrecognized",
                Vec::new(),
                Vec::new(),
                "No active tasks.\n",
                None,
                None,
                1,
                false,
                false,
                "heartbeat",
                None,
            )
            .expect("unknown level prompt should render");
        assert!(unknown.contains("Treat this epoch as observe-only."));
        assert!(unknown.contains("Record reusable discoveries"));
        assert!(!unknown.contains("The board and goal list are empty"));
    }

    #[test]
    fn autonomy_run_fragments_render() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");

        let retry = engine
            .render_system_autonomy_contract_retry()
            .expect("contract retry should render");
        assert_eq!(
            retry,
            "This autonomy epoch has no active work but has not called autonomy_complete. Call \
             autonomy_complete now with a concise final summary and one action per task actually \
             enriched, created, or executed. If no action was needed, use an empty actions list. \
             Do not start new work."
        );
    }

    #[test]
    fn memory_persistence_contract_retry_fragment_renders() {
        let engine = PromptEngine::new("en").expect("prompt engine should build");
        let retry = engine
            .render_system_memory_persistence_contract_retry()
            .expect("contract retry should render");
        assert!(retry.contains("memory_persistence_complete"));
    }
}
// to support multiple languages at compile time.
