//! `spacebot agent` — agent management over the control API.

use super::client::{self, ApiClient};
use super::output;
use clap::Subcommand;
use spacebot::api::agents::{IdentityResponse, WakeAgentResponse};
use spacebot::api::config::AgentConfigResponse;

#[derive(Subcommand)]
pub enum AgentCommand {
    /// List all configured agents
    List,
    /// Create a new agent and initialize it live
    Create {
        /// Agent ID (e.g. main)
        agent_id: String,
        /// Human-friendly display name
        #[arg(long)]
        display_name: Option<String>,
        /// Role description
        #[arg(short, long)]
        role: Option<String>,
    },
    /// Delete an agent and remove it from config
    Delete {
        /// Agent ID
        agent_id: String,
    },
    /// Show overview stats: memories, channels, cron jobs, bulletin
    Overview {
        /// Agent ID
        agent_id: String,
    },
    /// Manually wake a dormant agent
    Wake {
        /// Agent ID
        agent_id: String,
    },
    /// Manage identity files (SOUL.md, IDENTITY.md, ROLE.md)
    #[command(subcommand)]
    Identity(IdentityCommand),
    /// Manage resolved agent configuration
    #[command(subcommand)]
    Config(ConfigCommand),
}

#[derive(Subcommand)]
pub enum IdentityCommand {
    /// Print identity file contents
    Get {
        /// Agent ID
        agent_id: String,
    },
    /// Update identity files (only the sections you pass are written)
    Set {
        /// Agent ID
        agent_id: String,
        /// New SOUL.md content
        #[arg(long)]
        soul: Option<String>,
        /// New IDENTITY.md content
        #[arg(long)]
        identity: Option<String>,
        /// New ROLE.md content
        #[arg(long)]
        role: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show the resolved configuration
    Get {
        /// Agent ID
        agent_id: String,
    },
    /// Set a config value in config.toml (hot-reloaded)
    Set {
        /// Agent ID
        agent_id: String,
        /// Dotted key (e.g. tuning.max_turns, routing.worker)
        key: String,
        /// New value, parsed as JSON when possible (true, 50, ["a","b"]), else a string
        value: String,
    },
}

pub async fn run(ctx: &super::Context, agent_cmd: AgentCommand) -> anyhow::Result<()> {
    let client = ApiClient::from_context(ctx)?;

    match agent_cmd {
        AgentCommand::List => {
            let value = client.get("agents").await?;
            if ctx.json {
                output::json(&value);
                return Ok(());
            }
            let agents = value["agents"].as_array().cloned().unwrap_or_default();
            if agents.is_empty() {
                eprintln!("No agents configured.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = agents
                .iter()
                .map(|agent| {
                    vec![
                        agent["id"].as_str().unwrap_or("").to_string(),
                        agent["display_name"].as_str().unwrap_or("").to_string(),
                        agent["role"].as_str().unwrap_or("").to_string(),
                        agent["workspace"].as_str().unwrap_or("").to_string(),
                    ]
                })
                .collect();
            output::table(&["ID", "NAME", "ROLE", "WORKSPACE"], &rows);
            Ok(())
        }
        AgentCommand::Create {
            agent_id,
            display_name,
            role,
        } => {
            let mut body = serde_json::json!({ "agent_id": agent_id });
            if let Some(name) = &display_name {
                body["display_name"] = serde_json::json!(name);
            }
            if let Some(role) = &role {
                body["role"] = serde_json::json!(role);
            }

            let value = client.post("agents", &body).await?;
            if ctx.json {
                output::json(&value);
                return Ok(());
            }
            eprintln!("{}", value["message"].as_str().unwrap_or("Agent created."));
            Ok(())
        }
        AgentCommand::Delete { agent_id } => {
            let value = client
                .delete(&format!(
                    "agents?agent_id={}",
                    urlencoding::encode(&agent_id)
                ))
                .await?;
            if ctx.json {
                output::json(&value);
                return Ok(());
            }
            // The API reports a missing agent as success: false with a 200.
            if !value["success"].as_bool().unwrap_or(false) {
                anyhow::bail!(
                    "{}",
                    value["message"]
                        .as_str()
                        .unwrap_or("failed to delete agent")
                );
            }
            eprintln!("{}", value["message"].as_str().unwrap_or("Agent deleted."));
            Ok(())
        }
        AgentCommand::Overview { agent_id } => {
            let value = client
                .get(&format!(
                    "agents/overview?agent_id={}",
                    urlencoding::encode(&agent_id)
                ))
                .await?;
            if ctx.json {
                output::json(&value);
                return Ok(());
            }
            println!(
                "Memories:      {}",
                value["memory_total"].as_i64().unwrap_or(0)
            );
            if let Some(counts) = value["memory_counts"].as_object() {
                let mut entries: Vec<_> = counts.iter().collect();
                entries.sort_by_key(|(kind, _)| kind.as_str());
                for (kind, count) in entries {
                    println!(
                        "  {:<12} {}",
                        format!("{kind}:"),
                        count.as_i64().unwrap_or(0)
                    );
                }
            }
            println!(
                "Channels:      {}",
                value["channel_count"].as_i64().unwrap_or(0)
            );
            let cron_jobs = value["cron_jobs"].as_array().cloned().unwrap_or_default();
            println!("Cron jobs:     {}", cron_jobs.len());
            for job in &cron_jobs {
                let schedule = job["cron_expr"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!("every {}s", job["interval_secs"].as_u64().unwrap_or(0))
                    });
                let disabled = if job["enabled"].as_bool().unwrap_or(false) {
                    ""
                } else {
                    " (disabled)"
                };
                println!(
                    "  {}  {}  {}{}",
                    job["id"].as_str().unwrap_or(""),
                    schedule,
                    output::truncate(job["prompt"].as_str().unwrap_or(""), 60),
                    disabled,
                );
            }
            if let Some(ts) = value["last_bulletin_at"].as_str() {
                println!("Last bulletin: {}", output::short_timestamp(ts));
            }
            if let Some(bulletin) = value["latest_bulletin"].as_str() {
                println!();
                println!("{bulletin}");
            }
            Ok(())
        }
        AgentCommand::Wake { agent_id } => {
            let value = client
                .post(
                    &format!("agents/{}/wake", urlencoding::encode(&agent_id)),
                    &serde_json::json!({}),
                )
                .await?;
            if ctx.json {
                output::json(&value);
                return Ok(());
            }
            let result: WakeAgentResponse = client::parse(value)?;
            if result.fired {
                eprintln!("Wake queued for {}.", result.agent_id);
            } else {
                eprintln!("{}", result.message);
            }
            Ok(())
        }
        AgentCommand::Identity(identity_cmd) => match identity_cmd {
            IdentityCommand::Get { agent_id } => {
                let value = client
                    .get(&format!(
                        "agents/identity?agent_id={}",
                        urlencoding::encode(&agent_id)
                    ))
                    .await?;
                if ctx.json {
                    output::json(&value);
                    return Ok(());
                }
                let info: IdentityResponse = client::parse(value)?;
                let sections = [
                    ("SOUL.md", info.soul),
                    ("IDENTITY.md", info.identity),
                    ("ROLE.md", info.role),
                ];
                let mut printed = false;
                for (name, content) in sections {
                    if let Some(text) = content {
                        if printed {
                            println!();
                        }
                        println!("── {name} ──");
                        println!("{}", text.trim_end());
                        printed = true;
                    }
                }
                if !printed {
                    eprintln!("No identity files found for {agent_id}.");
                }
                Ok(())
            }
            IdentityCommand::Set {
                agent_id,
                soul,
                identity,
                role,
            } => {
                if soul.is_none() && identity.is_none() && role.is_none() {
                    anyhow::bail!("provide at least one of --soul, --identity, --role");
                }

                let mut body = serde_json::json!({ "agent_id": agent_id });
                if let Some(soul) = &soul {
                    body["soul"] = serde_json::json!(soul);
                }
                if let Some(identity) = &identity {
                    body["identity"] = serde_json::json!(identity);
                }
                if let Some(role) = &role {
                    body["role"] = serde_json::json!(role);
                }

                let value = client.put("agents/identity", &body).await?;
                if ctx.json {
                    output::json(&value);
                    return Ok(());
                }
                eprintln!("Identity updated for {agent_id}.");
                Ok(())
            }
        },
        AgentCommand::Config(config_cmd) => match config_cmd {
            ConfigCommand::Get { agent_id } => {
                let value = client
                    .get(&format!(
                        "agents/config?agent_id={}",
                        urlencoding::encode(&agent_id)
                    ))
                    .await?;
                if ctx.json {
                    output::json(&value);
                    return Ok(());
                }
                let config: AgentConfigResponse = client::parse(value)?;
                output::table(&["KEY", "VALUE"], &config_rows(config));
                Ok(())
            }
            ConfigCommand::Set {
                agent_id,
                key,
                value,
            } => {
                let Some((section, field)) = key.split_once('.') else {
                    anyhow::bail!("key must be section.field (e.g. tuning.max_turns)");
                };
                let parsed: serde_json::Value = serde_json::from_str(&value)
                    .unwrap_or(serde_json::Value::String(value.clone()));

                let body = serde_json::json!({
                    "agent_id": agent_id,
                    section: { field: parsed },
                });

                let value = client.put("agents/config", &body).await?;
                if ctx.json {
                    output::json(&value);
                    return Ok(());
                }
                // The server ignores unknown fields, so verify the key exists
                // in the resolved config it returned.
                let Some(new_value) = value.pointer(&format!("/{section}/{field}")) else {
                    anyhow::bail!("unknown config key '{key}'");
                };
                eprintln!("{key} = {new_value}");
                Ok(())
            }
        },
    }
}

/// Flatten the resolved config into dotted key/value rows.
fn config_rows(config: AgentConfigResponse) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut push = |key: &str, value: String| rows.push(vec![key.to_string(), value]);

    push("routing.channel", config.routing.channel);
    push("routing.branch", config.routing.branch);
    push("routing.worker", config.routing.worker);
    push("routing.compactor", config.routing.compactor);
    push("routing.cortex", config.routing.cortex);
    push("routing.voice", config.routing.voice);
    push(
        "routing.rate_limit_cooldown_secs",
        config.routing.rate_limit_cooldown_secs.to_string(),
    );

    push(
        "tuning.max_concurrent_branches",
        config.tuning.max_concurrent_branches.to_string(),
    );
    push(
        "tuning.max_concurrent_workers",
        config.tuning.max_concurrent_workers.to_string(),
    );
    push("tuning.max_turns", config.tuning.max_turns.to_string());
    push(
        "tuning.branch_max_turns",
        config.tuning.branch_max_turns.to_string(),
    );
    push(
        "tuning.context_window",
        config.tuning.context_window.to_string(),
    );
    push(
        "tuning.history_backfill_count",
        config.tuning.history_backfill_count.to_string(),
    );

    push("compaction.mode", config.compaction.mode.clone());
    push(
        "compaction.background_threshold",
        config.compaction.background_threshold.to_string(),
    );
    push(
        "compaction.aggressive_threshold",
        config.compaction.aggressive_threshold.to_string(),
    );
    push(
        "compaction.emergency_threshold",
        config.compaction.emergency_threshold.to_string(),
    );
    let chronicle = &config.compaction.chronicle;
    push(
        "compaction.chronicle.interval_messages",
        chronicle.interval_messages.to_string(),
    );
    push(
        "compaction.chronicle.interval_token_fraction",
        chronicle.interval_token_fraction.to_string(),
    );
    push(
        "compaction.chronicle.recent_window_hours",
        chronicle.recent_window_hours.to_string(),
    );
    push(
        "compaction.chronicle.max_recent",
        chronicle.max_recent.to_string(),
    );
    push(
        "compaction.chronicle.max_older",
        chronicle.max_older.to_string(),
    );
    push(
        "compaction.chronicle.context_token_budget",
        chronicle.context_token_budget.to_string(),
    );
    push(
        "compaction.chronicle.expand_message_limit",
        chronicle.expand_message_limit.to_string(),
    );
    push(
        "compaction.chronicle.max_messages_per_checkpoint",
        chronicle.max_messages_per_checkpoint.to_string(),
    );
    push(
        "compaction.chronicle.rollup_threshold",
        chronicle.rollup_threshold.to_string(),
    );
    push(
        "compaction.chronicle.rollup_batch",
        chronicle.rollup_batch.to_string(),
    );

    push(
        "cortex.tick_interval_secs",
        config.cortex.tick_interval_secs.to_string(),
    );
    push(
        "cortex.maintenance_interval_secs",
        config.cortex.maintenance_interval_secs.to_string(),
    );
    push(
        "cortex.worker_timeout_secs",
        config.cortex.worker_timeout_secs.to_string(),
    );
    push(
        "cortex.branch_timeout_secs",
        config.cortex.branch_timeout_secs.to_string(),
    );
    push(
        "cortex.detached_worker_timeout_retry_limit",
        config
            .cortex
            .detached_worker_timeout_retry_limit
            .to_string(),
    );
    push(
        "cortex.supervisor_kill_budget_per_tick",
        config.cortex.supervisor_kill_budget_per_tick.to_string(),
    );
    push(
        "cortex.circuit_breaker_threshold",
        config.cortex.circuit_breaker_threshold.to_string(),
    );
    push(
        "cortex.bulletin_interval_secs",
        config.cortex.bulletin_interval_secs.to_string(),
    );
    push(
        "cortex.bulletin_max_words",
        config.cortex.bulletin_max_words.to_string(),
    );
    push(
        "cortex.bulletin_max_turns",
        config.cortex.bulletin_max_turns.to_string(),
    );
    push(
        "cortex.maintenance_decay_rate",
        config.cortex.maintenance_decay_rate.to_string(),
    );
    push(
        "cortex.maintenance_prune_threshold",
        config.cortex.maintenance_prune_threshold.to_string(),
    );
    push(
        "cortex.maintenance_min_age_days",
        config.cortex.maintenance_min_age_days.to_string(),
    );
    push(
        "cortex.maintenance_merge_similarity_threshold",
        config
            .cortex
            .maintenance_merge_similarity_threshold
            .to_string(),
    );

    push("warmup.enabled", config.warmup.enabled.to_string());
    push(
        "warmup.eager_embedding_load",
        config.warmup.eager_embedding_load.to_string(),
    );
    push(
        "warmup.refresh_secs",
        config.warmup.refresh_secs.to_string(),
    );
    push(
        "warmup.startup_delay_secs",
        config.warmup.startup_delay_secs.to_string(),
    );

    push("coalesce.enabled", config.coalesce.enabled.to_string());
    push(
        "coalesce.debounce_ms",
        config.coalesce.debounce_ms.to_string(),
    );
    push(
        "coalesce.max_wait_ms",
        config.coalesce.max_wait_ms.to_string(),
    );
    push(
        "coalesce.min_messages",
        config.coalesce.min_messages.to_string(),
    );
    push(
        "coalesce.multi_user_only",
        config.coalesce.multi_user_only.to_string(),
    );

    push(
        "memory_persistence.enabled",
        config.memory_persistence.enabled.to_string(),
    );
    push(
        "memory_persistence.message_interval",
        config.memory_persistence.message_interval.to_string(),
    );

    push("browser.enabled", config.browser.enabled.to_string());
    push("browser.headless", config.browser.headless.to_string());
    push(
        "browser.evaluate_enabled",
        config.browser.evaluate_enabled.to_string(),
    );
    push(
        "browser.persist_session",
        config.browser.persist_session.to_string(),
    );
    push("browser.close_policy", config.browser.close_policy);

    push(
        "channel.listen_only_mode",
        config.channel.listen_only_mode.to_string(),
    );

    push("sandbox.mode", config.sandbox.mode);
    push(
        "sandbox.writable_paths",
        config.sandbox.writable_paths.join(","),
    );
    push(
        "sandbox.passthrough_env",
        config.sandbox.passthrough_env.join(","),
    );

    push(
        "projects.use_worktrees",
        config.projects.use_worktrees.to_string(),
    );
    push(
        "projects.worktree_name_template",
        config.projects.worktree_name_template,
    );
    push(
        "projects.auto_create_worktrees",
        config.projects.auto_create_worktrees.to_string(),
    );
    push(
        "projects.auto_discover_repos",
        config.projects.auto_discover_repos.to_string(),
    );
    push(
        "projects.auto_discover_worktrees",
        config.projects.auto_discover_worktrees.to_string(),
    );
    push(
        "projects.disk_usage_warning_threshold",
        config.projects.disk_usage_warning_threshold.to_string(),
    );

    push("discord.enabled", config.discord.enabled.to_string());
    push(
        "discord.allow_bot_messages",
        config.discord.allow_bot_messages.to_string(),
    );

    rows
}
