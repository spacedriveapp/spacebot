//! Identity file loading: SOUL.md, IDENTITY.md, USER.md.

use anyhow::Context as _;
use std::path::Path;

/// Loaded identity files for an agent.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub soul: Option<String>,
    pub identity: Option<String>,
    pub user: Option<String>,
}

impl Identity {
    /// Load identity files from an agent's workspace directory.
    pub async fn load(workspace: &Path) -> Self {
        Self {
            soul: load_optional_file(&workspace.join("SOUL.md")).await,
            identity: load_optional_file(&workspace.join("IDENTITY.md")).await,
            user: load_optional_file(&workspace.join("USER.md")).await,
        }
    }

    /// Render identity context for injection into system prompts.
    pub fn render(&self) -> String {
        let mut output = String::new();

        if let Some(soul) = &self.soul {
            output.push_str("## Soul\n\n");
            output.push_str(soul);
            output.push_str("\n\n");
        }
        if let Some(identity) = &self.identity {
            output.push_str("## Identity\n\n");
            output.push_str(identity);
            output.push_str("\n\n");
        }
        if let Some(user) = &self.user {
            output.push_str("## User\n\n");
            output.push_str(user);
            output.push_str("\n\n");
        }

        output
    }
}

/// Default identity file templates for new agents.
const DEFAULT_IDENTITY_FILES: &[(&str, &str)] = &[
    (
        "SOUL.md",
        "<!-- Define this agent's soul: personality, values, communication style, boundaries. -->\n",
    ),
    (
        "IDENTITY.md",
        "<!-- Define this agent's identity: name, nature, purpose. -->\n",
    ),
    (
        "USER.md",
        "<!-- Describe the human this agent interacts with: name, preferences, context. -->\n",
    ),
];

/// Write template identity files into an agent's workspace if they don't already exist.
///
/// Only writes files that are missing — existing files are left untouched.
pub async fn scaffold_identity_files(workspace: &Path) -> crate::error::Result<()> {
    for (filename, content) in DEFAULT_IDENTITY_FILES {
        let target = workspace.join(filename);
        if !target.exists() {
            tokio::fs::write(&target, content).await.with_context(|| {
                format!("failed to write identity template: {}", target.display())
            })?;
            tracing::info!(path = %target.display(), "wrote identity template");
        }
    }

    Ok(())
}

/// Load a file if it exists, returning None if missing.
async fn load_optional_file(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}
