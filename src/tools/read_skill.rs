//! Read skill tool — lets workers read the full content of a named skill.
//!
//! Workers see a listing of available skills (name + description) in their
//! system prompt. When they decide a skill is relevant to their task, they
//! call this tool to get the full instructions. This keeps the system prompt
//! compact while still giving workers on-demand access to any skill.

use crate::config::RuntimeConfig;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool that lets a worker read the full content of a named skill.
#[derive(Debug, Clone)]
pub struct ReadSkillTool {
    runtime_config: Arc<RuntimeConfig>,
}

impl ReadSkillTool {
    pub fn new(runtime_config: Arc<RuntimeConfig>) -> Self {
        Self { runtime_config }
    }
}

/// Error type for read_skill tool.
#[derive(Debug, thiserror::Error)]
#[error("read_skill failed: {0}")]
pub struct ReadSkillError(String);

/// Arguments for read_skill tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadSkillArgs {
    /// Name of the skill to read. Must match a name from the <available_skills> listing.
    pub name: String,
}

/// Output from read_skill tool.
#[derive(Debug, Serialize)]
pub struct ReadSkillOutput {
    /// The full skill instructions.
    pub content: String,
}

impl Tool for ReadSkillTool {
    const NAME: &'static str = "read_skill";

    type Error = ReadSkillError;
    type Args = ReadSkillArgs;
    type Output = ReadSkillOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read the full instructions for a skill by name. \
                Call this before starting any task that matches a skill in <available_skills>. \
                You may read multiple skills if the task requires more than one."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill name to read, exactly as it appears in <available_skills>."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let skills = self.runtime_config.skills.load();
        match skills.get(&args.name) {
            Some(skill) => Ok(ReadSkillOutput {
                content: skill.content.clone(),
            }),
            None => Err(ReadSkillError(format!(
                "skill '{}' not found. Available skills are listed in <available_skills> in your system prompt.",
                args.name
            ))),
        }
    }
}
