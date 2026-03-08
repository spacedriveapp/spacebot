use std::path::PathBuf;

use anyhow::Context as _;

use super::Config;

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

    // 1. Pick a provider
    let providers = &[
        "Anthropic",
        "OpenRouter",
        "OpenAI",
        "Z.ai (GLM)",
        "Groq",
        "Together AI",
        "Fireworks AI",
        "DeepSeek",
        "xAI (Grok)",
        "Mistral AI",
        "Gemini",
        "Ollama",
        "OpenCode Zen",
        "OpenCode Go",
        "MiniMax",
        "Moonshot AI (Kimi)",
        "Z.AI Coding Plan",
        "Kilo Gateway",
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

    let (provider_input_name, toml_key, provider_id) = match provider_idx {
        0 => ("Anthropic API key", "anthropic_key", "anthropic"),
        1 => ("OpenRouter API key", "openrouter_key", "openrouter"),
        2 => ("OpenAI API key", "openai_key", "openai"),
        3 => ("Z.ai (GLM) API key", "zhipu_key", "zhipu"),
        4 => ("Groq API key", "groq_key", "groq"),
        5 => ("Together AI API key", "together_key", "together"),
        6 => ("Fireworks AI API key", "fireworks_key", "fireworks"),
        7 => ("DeepSeek API key", "deepseek_key", "deepseek"),
        8 => ("xAI API key", "xai_key", "xai"),
        9 => ("Mistral API key", "mistral_key", "mistral"),
        10 => ("Google Gemini API key", "gemini_key", "gemini"),
        11 => ("Ollama base URL (optional)", "ollama_base_url", "ollama"),
        12 => ("OpenCode Zen API key", "opencode_zen_key", "opencode-zen"),
        13 => ("OpenCode Go API key", "opencode_go_key", "opencode-go"),
        14 => ("MiniMax API key", "minimax_key", "minimax"),
        15 => ("Moonshot API key", "moonshot_key", "moonshot"),
        16 => (
            "Z.AI Coding Plan API key",
            "zai_coding_plan_key",
            "zai-coding-plan",
        ),
        17 => ("Kilo Gateway API key", "kilo_key", "kilo"),
        _ => unreachable!(),
    };
    let is_secret = provider_id != "ollama";

    // 2. Get provider credential/endpoint (skip if OAuth was used)
    let provider_value = if anthropic_oauth.is_some() {
        // OAuth tokens are stored in anthropic_oauth.json, not in config.toml.
        // Use a placeholder so the config still has an [llm] section.
        String::new()
    } else if is_secret {
        let api_key: String = Password::new()
            .with_prompt(format!("Enter your {provider_input_name}"))
            .interact()?;

        let api_key = api_key.trim().to_string();
        if api_key.is_empty() {
            anyhow::bail!("API key cannot be empty");
        }
        api_key
    } else {
        let base_url: String = Input::new()
            .with_prompt(format!("Enter your {provider_input_name}"))
            .default("http://localhost:11434".to_string())
            .interact_text()?;

        let base_url = base_url.trim().to_string();
        if base_url.is_empty() {
            anyhow::bail!("Ollama base URL cannot be empty");
        }
        base_url
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
    config_content.push_str("[llm]\n");
    if anthropic_oauth.is_some() {
        config_content
            .push_str("# Anthropic authentication via OAuth (see anthropic_oauth.json)\n");
    } else {
        config_content.push_str(&format!("{toml_key} = \"{provider_value}\"\n"));
    }
    config_content.push('\n');

    // Write routing defaults for the chosen provider
    let routing = crate::llm::routing::defaults_for_provider(provider_id);
    config_content.push_str("[defaults.routing]\n");
    config_content.push_str(&format!("channel = \"{}\"\n", routing.channel));
    config_content.push_str(&format!("branch = \"{}\"\n", routing.branch));
    config_content.push_str(&format!("worker = \"{}\"\n", routing.worker));
    config_content.push_str(&format!("compactor = \"{}\"\n", routing.compactor));
    config_content.push_str(&format!("cortex = \"{}\"\n", routing.cortex));
    config_content.push('\n');

    config_content.push_str("[[agents]]\n");
    config_content.push_str(&format!("id = \"{agent_id}\"\n"));
    config_content.push_str("default = true\n");

    if let Some(discord) = &discord {
        config_content.push_str("\n[messaging.discord]\n");
        config_content.push_str("enabled = true\n");
        config_content.push_str(&format!("token = \"{}\"\n", discord.token));

        // Write the binding
        config_content.push_str("\n[[bindings]]\n");
        config_content.push_str(&format!("agent_id = \"{agent_id}\"\n"));
        config_content.push_str("channel = \"discord\"\n");
        if let Some(guild_id) = &discord.guild_id {
            config_content.push_str(&format!("guild_id = \"{guild_id}\"\n"));
        }
        if !discord.channel_ids.is_empty() {
            let ids: Vec<String> = discord
                .channel_ids
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect();
            config_content.push_str(&format!("channel_ids = [{}]\n", ids.join(", ")));
        }
        if !discord.dm_user_ids.is_empty() {
            let ids: Vec<String> = discord
                .dm_user_ids
                .iter()
                .map(|id| format!("\"{id}\""))
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
