//! Skill loading and prompt injection.
//!
//! Skills are directories containing a SKILL.md file with YAML frontmatter
//! (name, description, metadata) and a markdown body with instructions.
//! Compatible with OpenClaw's skill format and skills.sh registry.
//!
//! Skills are loaded from two sources (later wins on name conflicts):
//! 1. Instance-level: `{instance_dir}/skills/`
//! 2. Agent workspace: `{workspace}/skills/`
//!
//! The channel sees a summary of available skills and is instructed to
//! delegate skill work to workers. Workers receive the full skill content
//! in their system prompt.

mod installer;

pub use installer::{install_from_file, install_from_github};

use anyhow::Context as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A loaded skill definition.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Skill name from frontmatter.
    pub name: String,
    /// Short description from frontmatter.
    pub description: String,
    /// Absolute path to the SKILL.md file.
    pub file_path: PathBuf,
    /// Directory containing the skill.
    pub base_dir: PathBuf,
    /// Full rendered content (frontmatter stripped, `{baseDir}` resolved).
    pub content: String,
    /// Where this skill was loaded from.
    pub source: SkillSource,
}

/// Where a skill was loaded from, used for precedence tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// Instance-level `{instance_dir}/skills/`.
    Instance,
    /// Agent workspace `{workspace}/skills/`.
    Workspace,
}

/// All skills loaded for an agent.
#[derive(Debug, Clone, Default)]
pub struct SkillSet {
    /// Skills keyed by name (lowercase). Later sources override earlier ones.
    skills: HashMap<String, Skill>,
}

impl SkillSet {
    /// Load skills from instance and workspace directories.
    ///
    /// Workspace skills override instance skills with the same name.
    pub async fn load(instance_skills_dir: &Path, workspace_skills_dir: &Path) -> Self {
        let mut set = Self::default();

        // Instance skills (lowest precedence)
        if instance_skills_dir.is_dir()
            && let Ok(skills) =
                load_skills_from_dir(instance_skills_dir, SkillSource::Instance).await
        {
            for skill in skills {
                set.skills.insert(skill.name.to_lowercase(), skill);
            }
        }

        // Workspace skills (highest precedence, overrides instance)
        if workspace_skills_dir.is_dir()
            && let Ok(skills) =
                load_skills_from_dir(workspace_skills_dir, SkillSource::Workspace).await
        {
            for skill in skills {
                set.skills.insert(skill.name.to_lowercase(), skill);
            }
        }

        if !set.skills.is_empty() {
            tracing::info!(
                count = set.skills.len(),
                names = %set.skills.keys().cloned().collect::<Vec<_>>().join(", "),
                "skills loaded"
            );
        }

        set
    }

    /// Get a skill by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(&name.to_lowercase())
    }

    /// Iterate over all loaded skills.
    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    /// Number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Whether any skills are loaded.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Render the skills summary for injection into the channel system prompt.
    ///
    /// The channel sees skill names and descriptions but is instructed to
    /// delegate actual skill execution to workers.
    pub fn render_channel_prompt(&self, prompt_engine: &crate::prompts::PromptEngine) -> crate::error::Result<String> {
        if self.skills.is_empty() {
            return Ok(String::new());
        }

        let mut sorted_skills: Vec<&Skill> = self.skills.values().collect();
        sorted_skills.sort_by(|a, b| a.name.cmp(&b.name));

        let skill_infos: Vec<crate::prompts::SkillInfo> = sorted_skills
            .into_iter()
            .map(|s| crate::prompts::SkillInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                location: s.file_path.display().to_string(),
            })
            .collect();

        prompt_engine.render_skills_channel(skill_infos)
    }

    /// Render the skills section for injection into a worker system prompt.
    ///
    /// Workers get the full skill content so they can follow the instructions
    /// directly without needing to read the file.
    pub fn render_worker_prompt(
        &self,
        skill_name: &str,
        prompt_engine: &crate::prompts::PromptEngine,
    ) -> Option<String> {
        let skill = self.get(skill_name)?;

        match prompt_engine.render_skills_worker(&skill.name, &skill.content) {
            Ok(rendered) => Some(rendered),
            Err(error) => {
                tracing::error!(%error, skill = %skill_name, "failed to render worker skill prompt");
                None
            }
        }
    }

    /// Remove a skill by name.
    ///
    /// Returns the base directory path if the skill was found and removed.
    pub async fn remove(&mut self, name: &str) -> anyhow::Result<Option<PathBuf>> {
        let skill = match self.skills.remove(&name.to_lowercase()) {
            Some(s) => s,
            None => return Ok(None),
        };

        // Remove the skill directory from disk
        if skill.base_dir.exists() {
            tokio::fs::remove_dir_all(&skill.base_dir)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove skill directory: {}",
                        skill.base_dir.display()
                    )
                })?;

            tracing::info!(
                skill = %name,
                path = %skill.base_dir.display(),
                "skill removed"
            );
        }

        Ok(Some(skill.base_dir))
    }

    /// List all loaded skills with their metadata.
    pub fn list(&self) -> Vec<SkillInfo> {
        let mut skills: Vec<_> = self.skills.values().collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        skills
            .into_iter()
            .map(|s| SkillInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                file_path: s.file_path.clone(),
                base_dir: s.base_dir.clone(),
                source: s.source.clone(),
            })
            .collect()
    }
}

/// Public skill information for API responses.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub source: SkillSource,
}

/// Load all skills from a directory.
///
/// Each subdirectory containing a `SKILL.md` file is treated as a skill.
async fn load_skills_from_dir(dir: &Path, source: SkillSource) -> anyhow::Result<Vec<Skill>> {
    let mut skills = Vec::new();

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to read skills directory: {}", dir.display()))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }

        match load_skill(&skill_file, &path, source.clone()).await {
            Ok(skill) => {
                tracing::debug!(
                    name = %skill.name,
                    path = %skill_file.display(),
                    "loaded skill"
                );
                skills.push(skill);
            }
            Err(error) => {
                tracing::warn!(
                    path = %skill_file.display(),
                    %error,
                    "failed to load skill, skipping"
                );
            }
        }
    }

    Ok(skills)
}

/// Load a single skill from its SKILL.md file.
async fn load_skill(
    file_path: &Path,
    base_dir: &Path,
    source: SkillSource,
) -> anyhow::Result<Skill> {
    let raw = tokio::fs::read_to_string(file_path)
        .await
        .with_context(|| format!("failed to read {}", file_path.display()))?;

    let (frontmatter, body) = parse_frontmatter(&raw)?;

    let name = frontmatter.get("name").cloned().unwrap_or_else(|| {
        // Fall back to directory name if no name in frontmatter
        base_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    let description = frontmatter.get("description").cloned().unwrap_or_default();

    // Resolve {baseDir} template variable in the body
    let base_dir_str = base_dir.to_string_lossy();
    let content = body.replace("{baseDir}", &base_dir_str);

    Ok(Skill {
        name,
        description,
        file_path: file_path.to_path_buf(),
        base_dir: base_dir.to_path_buf(),
        content,
        source,
    })
}

/// Parse YAML frontmatter from a markdown file.
///
/// Expects `---` delimiters. Returns the frontmatter key-value pairs and
/// the remaining body. Compatible with OpenClaw's frontmatter format.
fn parse_frontmatter(content: &str) -> anyhow::Result<(HashMap<String, String>, String)> {
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        // No frontmatter, entire content is body
        return Ok((HashMap::new(), content.to_string()));
    }

    // Find the closing ---
    let after_opening = &trimmed[3..];
    let Some(end_pos) = after_opening.find("\n---") else {
        anyhow::bail!("unclosed frontmatter: missing closing ---");
    };

    let frontmatter_str = &after_opening[..end_pos].trim();
    let body_start = 3 + end_pos + 4; // skip opening --- + content + \n---
    let body = trimmed[body_start..].trim_start_matches('\n').to_string();

    // Parse the YAML frontmatter into simple key-value pairs.
    // We support the subset that OpenClaw uses: simple scalars and inline JSON for metadata.
    let mut map = HashMap::new();

    // Use serde_yaml-compatible line-based parsing for the simple cases
    // (name, description, homepage, user-invocable, etc.)
    // The metadata field can be complex JSON but we don't need to parse it — we just
    // need name and description for Spacebot's purposes.
    for line in frontmatter_str.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Handle simple `key: value` pairs
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim();

            // Skip complex multi-line values (metadata JSON blocks, etc.)
            if value.is_empty() || value.starts_with('{') || value.starts_with('[') {
                continue;
            }

            // Strip surrounding quotes
            let value = value
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .to_string();

            map.insert(key, value);
        }
    }

    Ok((map, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = indoc::indoc! {r#"
            ---
            name: weather
            description: Get current weather and forecasts (no API key required).
            homepage: https://wttr.in/:help
            ---

            # Weather

            Two free services, no API keys needed.
        "#};

        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("name").unwrap(), "weather");
        assert_eq!(
            fm.get("description").unwrap(),
            "Get current weather and forecasts (no API key required)."
        );
        assert_eq!(fm.get("homepage").unwrap(), "https://wttr.in/:help");
        assert!(body.starts_with("# Weather"));
    }

    #[test]
    fn test_parse_frontmatter_with_metadata_json() {
        let content = indoc::indoc! {r#"
            ---
            name: github
            description: "Interact with GitHub using the gh CLI."
            metadata: { "openclaw": { "emoji": "octopus", "requires": { "bins": ["gh"] } } }
            ---

            # GitHub Skill
        "#};

        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("name").unwrap(), "github");
        assert_eq!(
            fm.get("description").unwrap(),
            "Interact with GitHub using the gh CLI."
        );
        // metadata line is skipped (starts with {)
        assert!(!fm.contains_key("metadata"));
        assert!(body.starts_with("# GitHub Skill"));
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let content = "# Just a markdown file\n\nNo frontmatter here.";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn test_parse_frontmatter_quoted_description() {
        let content = indoc::indoc! {r#"
            ---
            name: test
            description: "A skill with 'quotes' inside"
            ---

            Body here.
        "#};

        let (fm, _body) = parse_frontmatter(content).unwrap();
        assert_eq!(
            fm.get("description").unwrap(),
            "A skill with 'quotes' inside"
        );
    }

    #[test]
    fn test_skill_set_channel_prompt_empty() {
        let set = SkillSet::default();
        let engine = crate::prompts::PromptEngine::new("en").unwrap();
        assert!(set.render_channel_prompt(&engine).unwrap().is_empty());
    }

    #[test]
    fn test_skill_set_channel_prompt() {
        let mut set = SkillSet::default();
        set.skills.insert(
            "weather".into(),
            Skill {
                name: "weather".into(),
                description: "Get weather forecasts".into(),
                file_path: PathBuf::from("/skills/weather/SKILL.md"),
                base_dir: PathBuf::from("/skills/weather"),
                content: "# Weather\n\nUse curl.".into(),
                source: SkillSource::Instance,
            },
        );

        let engine = crate::prompts::PromptEngine::new("en").unwrap();
        let prompt = set.render_channel_prompt(&engine).unwrap();
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>weather</name>"));
        assert!(prompt.contains("<description>Get weather forecasts</description>"));
    }

    #[test]
    fn test_skill_set_worker_prompt() {
        let mut set = SkillSet::default();
        set.skills.insert(
            "weather".into(),
            Skill {
                name: "weather".into(),
                description: "Get weather forecasts".into(),
                file_path: PathBuf::from("/skills/weather/SKILL.md"),
                base_dir: PathBuf::from("/skills/weather"),
                content: "# Weather\n\nUse curl.".into(),
                source: SkillSource::Instance,
            },
        );

        let engine = crate::prompts::PromptEngine::new("en").unwrap();
        let prompt = set.render_worker_prompt("weather", &engine).unwrap();
        assert!(prompt.contains("## Skill Instructions"));
        assert!(prompt.contains("**weather**"));
        assert!(prompt.contains("# Weather"));
        assert!(prompt.contains("Use curl."));

        assert!(set.render_worker_prompt("nonexistent", &engine).is_none());
    }
}
