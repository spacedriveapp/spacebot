//! Identity file loading: SOUL.md, IDENTITY.md, ROLE.md, SPEECH.md.
//!
//! Identity files live in the **agent root** directory (one level above the
//! workspace), which places them outside the sandbox boundary. This means
//! worker file tools cannot read or write them — all identity mutations must
//! go through the identity management API or factory tools.
//!
//! USER.md is deprecated — human context now lives on the org graph via
//! `HUMAN.md` files in `instance_dir/humans/{id}/` and is inherited by
//! linked agents automatically.

use anyhow::Context as _;
use std::path::Path;

/// Loaded identity files for an agent.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub soul: Option<String>,
    pub identity: Option<String>,
    pub role: Option<String>,
    pub speech: Option<String>,
}

impl Identity {
    /// Load identity files from the agent root directory.
    ///
    /// Identity files live at `instance_dir/agents/{id}/` — one level above
    /// the workspace — so they are outside the sandbox boundary and
    /// inaccessible to worker file tools.
    pub async fn load(identity_dir: &Path) -> Self {
        Self {
            soul: load_optional_file(&identity_dir.join("SOUL.md")).await,
            identity: load_optional_file(&identity_dir.join("IDENTITY.md")).await,
            role: load_optional_file(&identity_dir.join("ROLE.md")).await,
            speech: load_optional_file(&identity_dir.join("SPEECH.md")).await,
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
        if let Some(role) = &self.role {
            output.push_str("## Role\n\n");
            output.push_str(role);
            output.push_str("\n\n");
        }

        output
    }

    pub fn render_speech(&self) -> String {
        self.speech
            .as_ref()
            .map(|speech| {
                let mut output = String::from("## Speech Style\n\n");
                output.push_str(speech);
                output.push('\n');
                output
            })
            .unwrap_or_default()
    }
}

/// Default identity file templates for new agents.
///
/// Uses the `main-agent` preset content so fresh instances start with a
/// reasonable baseline rather than empty placeholder comments.
const DEFAULT_IDENTITY_FILES: &[(&str, &str)] = &[
    ("SOUL.md", include_str!("../../presets/main-agent/SOUL.md")),
    (
        "IDENTITY.md",
        include_str!("../../presets/main-agent/IDENTITY.md"),
    ),
    ("ROLE.md", include_str!("../../presets/main-agent/ROLE.md")),
    (
        "SPEECH.md",
        include_str!("../../presets/main-agent/SPEECH.md"),
    ),
];

/// Write template identity files into the agent root if they don't already exist.
///
/// Identity files live in the agent root directory (one level above the
/// workspace), outside the sandbox boundary.
///
/// Only writes files that are missing — existing files are left untouched.
pub async fn scaffold_identity_files(identity_dir: &Path) -> crate::error::Result<()> {
    // Ensure the identity directory exists (it should, but be safe).
    tokio::fs::create_dir_all(identity_dir)
        .await
        .with_context(|| {
            format!(
                "failed to create identity directory: {}",
                identity_dir.display()
            )
        })?;

    for (filename, content) in DEFAULT_IDENTITY_FILES {
        let target = identity_dir.join(filename);
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
