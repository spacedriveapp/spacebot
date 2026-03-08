//! Skills search tool — lets cortex search the skills.sh registry and list installed skills.
//!
//! Cortex chat needs to guide users through setting up integrations. This tool
//! provides two capabilities:
//!
//! 1. **Search the skills.sh registry** for skills matching a query (e.g. "github",
//!    "aws", "docker"). This lets cortex recommend skills for users to install.
//!
//! 2. **List installed skills** on the current agent so cortex can see what's
//!    already available.

use crate::config::RuntimeConfig;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const SKILLS_SH_SEARCH_URL: &str = "https://skills.sh/api/search";

/// Tool for searching skills.sh registry and listing installed skills.
#[derive(Debug, Clone)]
pub struct SkillsSearchTool {
    client: reqwest::Client,
    runtime_config: Arc<RuntimeConfig>,
}

impl SkillsSearchTool {
    pub fn new(runtime_config: Arc<RuntimeConfig>) -> Self {
        let client = reqwest::Client::builder()
            .gzip(true)
            .build()
            .expect("hardcoded reqwest client config");

        Self {
            client,
            runtime_config,
        }
    }
}

/// Error type for skills_search tool.
#[derive(Debug, thiserror::Error)]
pub enum SkillsSearchError {
    #[error("skills.sh search request failed: {0}")]
    RequestFailed(String),

    #[error("failed to parse skills.sh response: {0}")]
    InvalidResponse(String),
}

/// Arguments for skills_search tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SkillsSearchArgs {
    /// Action: `search` to search the skills.sh registry, or `installed` to list
    /// skills already installed on this agent.
    #[serde(default = "default_action")]
    pub action: String,

    /// Search query (required for `search` action). Examples: "github", "aws", "docker", "pdf".
    pub query: Option<String>,

    /// Maximum number of results to return for registry search (1-50, default 20).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_action() -> String {
    "search".to_string()
}

fn default_limit() -> u32 {
    20
}

/// A skill from the skills.sh registry.
#[derive(Debug, Serialize)]
pub struct RegistrySkillResult {
    /// GitHub source in owner/repo format.
    pub source: String,
    /// Skill identifier.
    pub skill_id: String,
    /// Display name.
    pub name: String,
    /// Total install count.
    pub installs: u64,
    /// How to install this skill.
    pub install_command: String,
}

/// An installed skill on this agent.
#[derive(Debug, Serialize)]
pub struct InstalledSkillResult {
    /// Skill name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Whether this is an instance-level or workspace-level skill.
    pub source: String,
}

/// Output from skills_search tool.
#[derive(Debug, Serialize)]
pub struct SkillsSearchOutput {
    pub success: bool,
    pub action: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub registry_results: Vec<RegistrySkillResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub installed_skills: Vec<InstalledSkillResult>,
}

// -- skills.sh API response types (private) --

#[derive(Debug, Deserialize)]
struct UpstreamSearchResponse {
    skills: Vec<UpstreamSkill>,
    count: usize,
    #[allow(dead_code)]
    query: String,
}

#[derive(Debug, Deserialize)]
struct UpstreamSkill {
    source: String,
    #[serde(rename = "skillId")]
    skill_id: String,
    name: String,
    #[serde(default)]
    installs: u64,
}

impl Tool for SkillsSearchTool {
    const NAME: &'static str = "skills_search";

    type Error = SkillsSearchError;
    type Args = SkillsSearchArgs;
    type Output = SkillsSearchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/skills_search").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["search", "installed"],
                        "default": "search",
                        "description": "Use `search` to find skills on skills.sh, or `installed` to list skills already installed on this agent."
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query for the skills.sh registry. Examples: \"github\", \"aws\", \"docker\", \"pdf\". Required for `search` action."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 20,
                        "description": "Maximum number of registry results to return."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let action = args.action.trim().to_ascii_lowercase();

        match action.as_str() {
            "search" => {
                let query = args
                    .query
                    .as_deref()
                    .map(str::trim)
                    .filter(|query| query.len() >= 2);

                let Some(query) = query else {
                    return Ok(SkillsSearchOutput {
                        success: false,
                        action,
                        message: "query is required for search and must be at least 2 characters"
                            .to_string(),
                        error: Some(
                            "query is required for search and must be at least 2 characters"
                                .to_string(),
                        ),
                        registry_results: Vec::new(),
                        installed_skills: Vec::new(),
                    });
                };

                let limit = args.limit.clamp(1, 50);

                let response = self
                    .client
                    .get(SKILLS_SH_SEARCH_URL)
                    .query(&[("q", query), ("limit", &limit.to_string())])
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await
                    .map_err(|error| SkillsSearchError::RequestFailed(error.to_string()))?;

                if !response.status().is_success() {
                    let body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "failed to read response body".into());
                    return Err(SkillsSearchError::RequestFailed(format!(
                        "HTTP {}: {}",
                        body.len(),
                        body
                    )));
                }

                let body: UpstreamSearchResponse = response
                    .json()
                    .await
                    .map_err(|error| SkillsSearchError::InvalidResponse(error.to_string()))?;

                let results: Vec<RegistrySkillResult> = body
                    .skills
                    .into_iter()
                    .map(|skill| RegistrySkillResult {
                        install_command: format!("spacebot skill add {}", skill.source),
                        source: skill.source,
                        skill_id: skill.skill_id,
                        name: skill.name,
                        installs: skill.installs,
                    })
                    .collect();

                let result_count = results.len();

                Ok(SkillsSearchOutput {
                    success: true,
                    action,
                    message: format!(
                        "Found {} skills matching '{}' on skills.sh ({} total on registry)",
                        result_count, query, body.count
                    ),
                    error: None,
                    registry_results: results,
                    installed_skills: Vec::new(),
                })
            }
            "installed" => {
                let skills = self.runtime_config.skills.load();
                let installed: Vec<InstalledSkillResult> = skills
                    .list()
                    .into_iter()
                    .map(|skill| InstalledSkillResult {
                        name: skill.name,
                        description: skill.description,
                        source: match skill.source {
                            crate::skills::SkillSource::Instance => "instance".to_string(),
                            crate::skills::SkillSource::Workspace => "workspace".to_string(),
                        },
                    })
                    .collect();

                let count = installed.len();

                Ok(SkillsSearchOutput {
                    success: true,
                    action,
                    message: if count == 0 {
                        "No skills installed. Use skills_search(action=\"search\", query=\"...\") to find skills on skills.sh, then install via the dashboard or CLI.".to_string()
                    } else {
                        format!("{} skills installed", count)
                    },
                    error: None,
                    registry_results: Vec::new(),
                    installed_skills: installed,
                })
            }
            other => Ok(SkillsSearchOutput {
                success: false,
                action: other.to_string(),
                message: format!("invalid action '{other}'. Valid actions: search, installed"),
                error: Some(format!(
                    "invalid action '{other}'. Valid actions: search, installed"
                )),
                registry_results: Vec::new(),
                installed_skills: Vec::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_action_is_search() {
        let args: SkillsSearchArgs = serde_json::from_str(r#"{"query": "github"}"#).unwrap();
        assert_eq!(args.action, "search");
    }

    #[test]
    fn default_limit_is_20() {
        let args: SkillsSearchArgs = serde_json::from_str(r#"{"query": "github"}"#).unwrap();
        assert_eq!(args.limit, 20);
    }
}
