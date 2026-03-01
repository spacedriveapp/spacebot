//! Tool for workers to store secrets in the instance-level secret store.
//!
//! Useful for autonomous workflows where a worker creates accounts, generates
//! API keys, or obtains credentials that should be persisted for future use.

use crate::secrets::store::{SecretCategory, SecretsStore, auto_categorize};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for storing secrets from worker subprocesses.
#[derive(Debug, Clone)]
pub struct SecretSetTool {
    secrets_store: Arc<SecretsStore>,
}

impl SecretSetTool {
    /// Create a new secret set tool with access to the instance-level store.
    pub fn new(secrets_store: Arc<SecretsStore>) -> Self {
        Self { secrets_store }
    }
}

/// Error type for secret set tool.
#[derive(Debug, thiserror::Error)]
#[error("Failed to set secret: {0}")]
pub struct SecretSetError(String);

/// Arguments for secret set tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SecretSetArgs {
    /// The name of the secret (e.g. "GH_TOKEN", "STRIPE_API_KEY").
    /// Use UPPER_SNAKE_CASE by convention.
    pub name: String,
    /// The secret value to store.
    pub value: String,
    /// Optional category override: "system" or "tool".
    /// If omitted, the category is auto-assigned based on the name.
    /// Most worker-created secrets should be "tool" (exposed to subprocesses).
    pub category: Option<String>,
}

/// Output from secret set tool.
#[derive(Debug, Serialize)]
pub struct SecretSetOutput {
    /// Whether the secret was stored successfully.
    pub success: bool,
    /// The name of the secret that was stored.
    pub name: String,
    /// The category that was assigned.
    pub category: String,
    /// Whether this was an update to an existing secret.
    pub updated: bool,
}

impl Tool for SecretSetTool {
    const NAME: &'static str = "secret_set";

    type Error = SecretSetError;
    type Args = SecretSetArgs;
    type Output = SecretSetOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/secret_set").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Secret name in UPPER_SNAKE_CASE (e.g. GH_TOKEN, STRIPE_API_KEY)"
                    },
                    "value": {
                        "type": "string",
                        "description": "The secret value to store"
                    },
                    "category": {
                        "type": "string",
                        "enum": ["system", "tool"],
                        "description": "Optional category override. Defaults to auto-categorization based on the name. 'tool' secrets are exposed as env vars in future worker subprocesses. 'system' secrets are only accessible internally."
                    }
                },
                "required": ["name", "value"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let name = args.name.trim().to_uppercase();

        if name.is_empty() {
            return Err(SecretSetError("secret name cannot be empty".to_string()));
        }

        // Secret names are injected as environment variables, so they must be
        // valid env var identifiers: start with a letter, contain only uppercase
        // letters, digits, and underscores.
        if !name.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
            || !name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(SecretSetError(
                "secret name must be a valid env var name (A-Z, 0-9, _ only, starting with a letter)".to_string(),
            ));
        }

        if args.value.is_empty() {
            return Err(SecretSetError("secret value cannot be empty".to_string()));
        }

        // Determine category: explicit override or auto-categorize.
        let category = match args.category.as_deref() {
            Some("system") => SecretCategory::System,
            Some("tool") => SecretCategory::Tool,
            Some(other) => {
                return Err(SecretSetError(format!(
                    "invalid category '{other}' — must be 'system' or 'tool'"
                )));
            }
            None => auto_categorize(&name),
        };

        // Check if this is an update to an existing secret.
        let updated = self.secrets_store.get_metadata(&name).is_ok();

        self.secrets_store
            .set(&name, &args.value, category)
            .map_err(|error| SecretSetError(format!("{error}")))?;

        tracing::info!(
            name = %name,
            category = %category,
            updated,
            "worker stored secret via secret_set tool"
        );

        Ok(SecretSetOutput {
            success: true,
            name,
            category: category.to_string(),
            updated,
        })
    }
}
