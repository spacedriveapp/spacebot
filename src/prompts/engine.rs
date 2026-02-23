use crate::error::Result;
use anyhow::Context;
use minijinja::{Environment, Value, context};
use std::collections::HashMap;
use std::sync::Arc;

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
        env.add_template("branch", crate::prompts::text::get("branch"))?;
        env.add_template("worker", crate::prompts::text::get("worker"))?;
        env.add_template("cortex", crate::prompts::text::get("cortex"))?;
        env.add_template(
            "cortex_bulletin",
            crate::prompts::text::get("cortex_bulletin"),
        )?;
        env.add_template("compactor", crate::prompts::text::get("compactor"))?;
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
            "fragments/available_channels",
            crate::prompts::text::get("fragments/available_channels"),
        )?;

        // System message fragments
        env.add_template(
            "fragments/system/retrigger",
            crate::prompts::text::get("fragments/system/retrigger"),
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
            "fragments/system/cortex_synthesis",
            crate::prompts::text::get("fragments/system/cortex_synthesis"),
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
    /// ```rust
    /// let ctx = context! {
    ///     identity_context => "Some identity text",
    ///     browser_enabled => true,
    /// };
    /// let rendered = engine.render("channel", ctx)?;
    /// ```
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

    /// Convenience method for rendering worker capabilities fragment.
    pub fn render_worker_capabilities(
        &self,
        browser_enabled: bool,
        web_search_enabled: bool,
        opencode_enabled: bool,
    ) -> Result<String> {
        self.render(
            "fragments/worker_capabilities",
            context! {
                browser_enabled => browser_enabled,
                web_search_enabled => web_search_enabled,
                opencode_enabled => opencode_enabled,
            },
        )
    }

    /// Convenience method for rendering conversation context fragment.
    pub fn render_conversation_context(
        &self,
        platform: &str,
        server_name: Option<&str>,
        channel_name: Option<&str>,
    ) -> Result<String> {
        self.render(
            "fragments/conversation_context",
            context! {
                platform => platform,
                server_name => server_name,
                channel_name => channel_name,
            },
        )
    }

    /// Convenience method for rendering skills channel fragment.
    pub fn render_skills_channel(&self, skills: Vec<SkillInfo>) -> Result<String> {
        self.render(
            "fragments/skills_channel",
            context! {
                skills => skills,
            },
        )
    }

    /// Render the worker system prompt with filesystem context.
    pub fn render_worker_prompt(&self, instance_dir: &str, workspace_dir: &str) -> Result<String> {
        self.render(
            "worker",
            context! {
                instance_dir => instance_dir,
                workspace_dir => workspace_dir,
            },
        )
    }

    /// Render the branch system prompt with filesystem context.
    pub fn render_branch_prompt(&self, instance_dir: &str, workspace_dir: &str) -> Result<String> {
        self.render(
            "branch",
            context! {
                instance_dir => instance_dir,
                workspace_dir => workspace_dir,
            },
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
    pub fn render_skills_worker(&self, skills: Vec<SkillInfo>) -> Result<String> {
        self.render(
            "fragments/skills_worker",
            context! {
                skills => skills,
            },
        )
    }

    /// Convenience method for rendering system retrigger message.
    pub fn render_system_retrigger(&self) -> Result<String> {
        self.render_static("fragments/system/retrigger")
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

    /// Render the profile synthesis prompt with identity and bulletin context.
    pub fn render_system_profile_synthesis(
        &self,
        identity_context: Option<&str>,
        memory_bulletin: Option<&str>,
    ) -> Result<String> {
        self.render(
            "fragments/system/profile_synthesis",
            context! {
                identity_context => identity_context,
                memory_bulletin => memory_bulletin,
            },
        )
    }

    /// Convenience method for rendering cortex synthesis prompt.
    pub fn render_system_cortex_synthesis(
        &self,
        max_words: usize,
        raw_sections: &str,
    ) -> Result<String> {
        self.render(
            "fragments/system/cortex_synthesis",
            context! {
                max_words => max_words,
                raw_sections => raw_sections,
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

    /// Render the complete channel system prompt with all dynamic components.
    #[allow(clippy::too_many_arguments)]
    pub fn render_channel_prompt(
        &self,
        identity_context: Option<String>,
        memory_bulletin: Option<String>,
        skills_prompt: Option<String>,
        worker_capabilities: String,
        conversation_context: Option<String>,
        status_text: Option<String>,
        coalesce_hint: Option<String>,
        available_channels: Option<String>,
    ) -> Result<String> {
        self.render(
            "channel",
            context! {
                identity_context => identity_context,
                memory_bulletin => memory_bulletin,
                skills_prompt => skills_prompt,
                worker_capabilities => worker_capabilities,
                conversation_context => conversation_context,
                status_text => status_text,
                coalesce_hint => coalesce_hint,
                available_channels => available_channels,
            },
        )
    }

    /// Render the cortex chat system prompt with optional channel context.
    pub fn render_cortex_chat_prompt(
        &self,
        identity_context: Option<String>,
        memory_bulletin: Option<String>,
        channel_transcript: Option<String>,
        worker_capabilities: String,
    ) -> Result<String> {
        self.render(
            "cortex_chat",
            context! {
                identity_context => identity_context,
                memory_bulletin => memory_bulletin,
                channel_transcript => channel_transcript,
                worker_capabilities => worker_capabilities,
            },
        )
    }

    /// Get the configured language code.
    pub fn language(&self) -> &str {
        &self.language
    }
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
}

/// Information about a channel for template rendering.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelEntry {
    pub name: String,
    pub platform: String,
    pub id: String,
}

// All templates are now loaded from the centralized text registry (src/prompts/text.rs)
// to support multiple languages at compile time.
