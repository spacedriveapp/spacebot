use std::path::PathBuf;

use anyhow::Context as _;

use super::Config;

/// Escape a string for embedding in a TOML basic string literal. The config
/// writer below interpolates user input (API keys, URLs, IDs) with `format!`,
/// so a quote or backslash would corrupt the file and break the next boot.
fn toml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if (ch as u32) < 0x20 || ch as u32 == 0x7f => {
                escaped.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// Interactive first-run onboarding. Creates ~/.spacebot with a minimal config.
///
/// Returns `Some(path)` if the CLI wizard created a config file, or `None` if
/// the user chose to set up via the embedded UI (setup mode).
pub fn run_onboarding() -> anyhow::Result<Option<PathBuf>> {
    use dialoguer::{Input, Password, Select};
    use std::io::Write;

    println!();
    println!("  Welcome to Spacebot");
    println!("  -------------------");
    println!();
    println!("  No configuration found. Let's set things up.");
    println!();

    let setup_method = Select::new()
        .with_prompt("How do you want to set up?")
        .items(&["Set up here (CLI)", "Set up in the browser (localhost)"])
        .default(0)
        .interact()?;

    if setup_method == 1 {
        // Write a skeleton config so that subsequent read-modify-write cycles
        // (e.g. adding a provider key via the UI) preserve the default entries.
        let instance_dir = Config::default_instance_dir();
        std::fs::create_dir_all(&instance_dir)
            .with_context(|| format!("failed to create {}", instance_dir.display()))?;
        let config_path = instance_dir.join("config.toml");
        if !config_path.exists() {
            write_skeleton_config(&config_path, "main")?;
        }

        println!();
        println!("  Starting in setup mode. Open the UI to finish configuration:");
        println!();
        println!("    http://localhost:19898");
        println!();
        return Ok(Some(config_path));
    }

    println!();

    // 1. Pick a provider. There are two: Anthropic natively, or any
    // OpenAI-compatible endpoint. LiteLLM is the recommended way to reach
    // anything else, but it is a suggestion, not a dependency.
    let providers = &[
        "Anthropic",
        "OpenAI-compatible endpoint (LiteLLM, vLLM, Ollama, OpenRouter, ...)",
    ];
    let provider_idx = Select::new()
        .with_prompt("Which LLM provider do you want to use?")
        .items(providers)
        .default(0)
        .interact()?;

    // For Anthropic, offer OAuth login as an option
    let anthropic_oauth = if provider_idx == 0 {
        let auth_method = Select::new()
            .with_prompt("How do you want to authenticate with Anthropic?")
            .items(&[
                "Log in with Claude Pro/Max (OAuth)",
                "Log in via API Console (OAuth)",
                "Enter an API key manually",
            ])
            .default(0)
            .interact()?;

        if auth_method <= 1 {
            let mode = if auth_method == 0 {
                crate::auth::AuthMode::Max
            } else {
                crate::auth::AuthMode::Console
            };
            let instance_dir = Config::default_instance_dir();
            std::fs::create_dir_all(&instance_dir)?;

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .with_context(|| "failed to build tokio runtime")?;

            runtime.block_on(crate::auth::login_interactive(&instance_dir, mode))?;
            Some(true)
        } else {
            None
        }
    } else {
        None
    };

    // 2. Provider endpoint + credential.
    let (provider_id, api_type, base_url, api_key) = if provider_idx == 0 {
        let api_key = if anthropic_oauth.is_some() {
            // OAuth tokens live in the credentials file, not config.toml.
            String::new()
        } else {
            let api_key: String = Password::new()
                .with_prompt("Enter your Anthropic API key")
                .interact()?;
            let api_key = api_key.trim().to_string();
            if api_key.is_empty() {
                anyhow::bail!("API key cannot be empty");
            }
            api_key
        };
        (
            "anthropic".to_string(),
            "anthropic",
            crate::config::ANTHROPIC_PROVIDER_BASE_URL.to_string(),
            api_key,
        )
    } else {
        let provider_id: String = Input::new()
            .with_prompt("Provider name (used as the model prefix, e.g. litellm/claude-sonnet-4)")
            .default("litellm".to_string())
            .interact_text()?;
        let provider_id = provider_id.trim().to_lowercase().replace(' ', "-");
        if provider_id.is_empty() {
            anyhow::bail!("Provider name cannot be empty");
        }
        // The name lands in a TOML table header (`[llm.provider.<name>]`), so
        // it must stay a bare key — escaping can't help there.
        if !provider_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            anyhow::bail!(
                "Provider name may only contain letters, numbers, dashes, and underscores"
            );
        }

        // base_url is the full path prefix — nothing is appended but the
        // endpoint, so most OpenAI-compatible servers need the trailing /v1.
        let base_url: String = Input::new()
            .with_prompt("Base URL (full path prefix, including /v1 if required)")
            .default("http://localhost:4000/v1".to_string())
            .interact_text()?;
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            anyhow::bail!("Base URL cannot be empty");
        }

        let api_key: String = Password::new()
            .with_prompt("API key (leave blank if the endpoint needs none)")
            .allow_empty_password(true)
            .interact()?;

        (
            provider_id,
            "openai_compatible",
            base_url,
            api_key.trim().to_string(),
        )
    };

    // 3. Agent name
    let agent_id: String = Input::new()
        .with_prompt("Agent name")
        .default("main".to_string())
        .interact_text()?;

    let agent_id = agent_id.trim().to_lowercase().replace(' ', "-");

    // 4. Optional Discord setup
    let setup_discord = Select::new()
        .with_prompt("Set up Discord integration?")
        .items(&["Not now", "Yes"])
        .default(0)
        .interact()?;

    struct DiscordSetup {
        token: String,
        guild_id: Option<String>,
        channel_ids: Vec<String>,
        dm_user_ids: Vec<String>,
    }

    let discord = if setup_discord == 1 {
        let token: String = Password::new()
            .with_prompt("Discord bot token")
            .interact()?;
        let token = token.trim().to_string();

        if token.is_empty() {
            None
        } else {
            println!();
            println!("  Tip: Right-click a server or channel in Discord with");
            println!("  Developer Mode enabled to copy IDs. Leave blank to skip.");
            println!();

            let guild_id: String = Input::new()
                .with_prompt("Server (guild) ID")
                .allow_empty(true)
                .default(String::new())
                .interact_text()?;
            let guild_id = guild_id.trim().to_string();
            let guild_id = if guild_id.is_empty() {
                None
            } else {
                Some(guild_id)
            };

            let channel_ids_raw: String = Input::new()
                .with_prompt("Channel IDs (comma-separated, or blank for all)")
                .allow_empty(true)
                .default(String::new())
                .interact_text()?;
            let channel_ids: Vec<String> = channel_ids_raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            let dm_user_ids_raw: String = Input::new()
                .with_prompt("User IDs allowed to DM the bot (comma-separated, or blank)")
                .allow_empty(true)
                .default(String::new())
                .interact_text()?;
            let dm_user_ids: Vec<String> = dm_user_ids_raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            Some(DiscordSetup {
                token,
                guild_id,
                channel_ids,
                dm_user_ids,
            })
        }
    } else {
        None
    };

    // 5. Build config.toml
    let instance_dir = Config::default_instance_dir();
    let config_path = instance_dir.join("config.toml");

    // Create directory structure
    std::fs::create_dir_all(&instance_dir)
        .with_context(|| format!("failed to create {}", instance_dir.display()))?;

    let mut config_content = String::new();
    config_content.push_str(&format!("[llm.provider.{provider_id}]\n"));
    config_content.push_str(&format!("api_type = \"{}\"\n", toml_escape(api_type)));
    config_content.push_str(&format!("base_url = \"{}\"\n", toml_escape(&base_url)));
    if anthropic_oauth.is_some() {
        config_content.push_str("# Authenticated via OAuth — see `spacebot auth login`\n");
        config_content.push_str("api_key = \"\"\n");
    } else {
        config_content.push_str(&format!("api_key = \"{}\"\n", toml_escape(&api_key)));
    }
    config_content.push('\n');

    // Routing defaults. Only Anthropic has model names we can pick for the
    // user; an OpenAI-compatible endpoint serves whatever its operator
    // configured, so leave [defaults.routing] out entirely and let config
    // load infer an empty routing rather than writing a model that 404s.
    let routing = crate::llm::routing::defaults_for_provider(&provider_id);
    if routing.channel.is_empty() {
        config_content.push_str(&format!(
            "# [defaults.routing]\n\
             # No routing written: \"{provider_id}\" serves its own model names.\n\
             # Set each role to a model your endpoint serves, e.g.\n\
             # channel = \"{provider_id}/claude-sonnet-4\"\n\n"
        ));
    } else {
        config_content.push_str("[defaults.routing]\n");
        config_content.push_str(&format!("channel = \"{}\"\n", routing.channel));
        config_content.push_str(&format!("branch = \"{}\"\n", routing.branch));
        config_content.push_str(&format!("worker = \"{}\"\n", routing.worker));
        config_content.push_str(&format!("compactor = \"{}\"\n", routing.compactor));
        config_content.push_str(&format!("cortex = \"{}\"\n", routing.cortex));
        config_content.push('\n');
    }

    config_content.push_str("[[agents]]\n");
    config_content.push_str(&format!("id = \"{}\"\n", toml_escape(&agent_id)));
    config_content.push_str("default = true\n");

    if let Some(discord) = &discord {
        config_content.push_str("\n[messaging.discord]\n");
        config_content.push_str("enabled = true\n");
        config_content.push_str(&format!("token = \"{}\"\n", toml_escape(&discord.token)));

        // Write the binding
        config_content.push_str("\n[[bindings]]\n");
        config_content.push_str(&format!("agent_id = \"{}\"\n", toml_escape(&agent_id)));
        config_content.push_str("channel = \"discord\"\n");
        if let Some(guild_id) = &discord.guild_id {
            config_content.push_str(&format!("guild_id = \"{}\"\n", toml_escape(guild_id)));
        }
        if !discord.channel_ids.is_empty() {
            let ids: Vec<String> = discord
                .channel_ids
                .iter()
                .map(|id| format!("\"{}\"", toml_escape(id)))
                .collect();
            config_content.push_str(&format!("channel_ids = [{}]\n", ids.join(", ")));
        }
        if !discord.dm_user_ids.is_empty() {
            let ids: Vec<String> = discord
                .dm_user_ids
                .iter()
                .map(|id| format!("\"{}\"", toml_escape(id)))
                .collect();
            config_content.push_str(&format!("dm_allowed_users = [{}]\n", ids.join(", ")));
        }
    }

    let mut file = std::fs::File::create(&config_path)
        .with_context(|| format!("failed to create {}", config_path.display()))?;
    file.write_all(config_content.as_bytes())?;

    println!();
    println!("  Config written to {}", config_path.display());
    println!("  Agent '{}' created.", agent_id);
    println!();
    println!("  You can customize identity files in:");
    println!(
        "    {}/agents/{}/workspace/",
        instance_dir.display(),
        agent_id
    );
    println!();

    Ok(Some(config_path))
}

/// Write a minimal config.toml with the default agent, admin human, and link.
fn write_skeleton_config(config_path: &std::path::Path, agent_id: &str) -> anyhow::Result<()> {
    #[derive(serde::Serialize)]
    struct Skeleton<'a> {
        agents: Vec<SkeletonAgent<'a>>,
        humans: Vec<SkeletonHuman<'a>>,
        links: Vec<SkeletonLink<'a>>,
    }
    #[derive(serde::Serialize)]
    struct SkeletonAgent<'a> {
        id: &'a str,
    }
    #[derive(serde::Serialize)]
    struct SkeletonHuman<'a> {
        id: &'a str,
    }
    #[derive(serde::Serialize)]
    struct SkeletonLink<'a> {
        from: &'a str,
        to: &'a str,
        direction: &'a str,
        kind: &'a str,
    }

    let skeleton = Skeleton {
        agents: vec![SkeletonAgent { id: agent_id }],
        humans: vec![SkeletonHuman { id: "admin" }],
        links: vec![SkeletonLink {
            from: "admin",
            to: agent_id,
            direction: "one_way",
            kind: "hierarchical",
        }],
    };

    let content =
        toml::to_string_pretty(&skeleton).with_context(|| "failed to serialize skeleton config")?;
    std::fs::write(config_path, content)
        .with_context(|| format!("failed to write {}", config_path.display()))
}

#[cfg(test)]
mod tests {
    use super::toml_escape;

    #[test]
    fn escaped_credentials_round_trip_through_toml() {
        // A credential containing quotes, backslashes, and a newline must not
        // corrupt the config file — the parsed value must equal the original.
        let nasty = "sk-\"quoted\"\\path\nwith\ttabs\r\nand\u{7}bell";
        let document = format!("api_key = \"{}\"\n", toml_escape(nasty));
        let parsed: toml::Value = toml::from_str(&document).expect("escaped config must parse");
        assert_eq!(parsed["api_key"].as_str(), Some(nasty));
    }

    #[test]
    fn control_characters_are_unicode_escaped() {
        assert_eq!(toml_escape("\u{0}\u{1f}\u{7f}"), "\\u0000\\u001F\\u007F");
    }
}
