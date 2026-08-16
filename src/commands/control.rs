//! Router-side control plane for slash commands.
//!
//! Control commands execute deterministically against channel state — the
//! settings stores, process control, and runtime config — without creating
//! an inbound message or consuming an agent turn. Dispatching here, before
//! the channel's message queue, is what makes them busy-immune: a `/quiet`
//! lands even while the channel is mid-turn.
//!
//! The reply-text builders are shared with the channel's in-queue dispatch
//! path so both surfaces produce identical output.

use super::registry::{ControlAction, Surface};
use crate::ProcessType;
use crate::agent::channel_prompt::TemporalContext;
use crate::conversation::settings::{ResolvedConversationSettings, ResponseMode};

/// Confirmation reply for a response-mode change.
pub fn mode_confirmation(mode: ResponseMode) -> &'static str {
    match mode {
        ResponseMode::Active => "active mode enabled. i'll respond normally in this chat.",
        ResponseMode::Observe => {
            "observe mode enabled. i'll learn from this conversation but won't respond."
        }
        ResponseMode::MentionOnly => {
            "mention-only mode enabled. i'll only respond when @mentioned or replied to."
        }
    }
}

/// Short mode name for failure replies.
fn mode_name(mode: ResponseMode) -> &'static str {
    match mode {
        ResponseMode::Active => "active",
        ResponseMode::Observe => "observe",
        ResponseMode::MentionOnly => "mention-only",
    }
}

/// Human-readable label for a response mode in `/status` output.
pub fn mode_label(mode: ResponseMode) -> &'static str {
    match mode {
        ResponseMode::Active => "active",
        ResponseMode::Observe => "observe (learning, never responds)",
        ResponseMode::MentionOnly => "mention-only (@mention/reply only)",
    }
}

/// `/status` reply body.
#[allow(clippy::too_many_arguments)]
pub fn status_text(
    agent_id: &str,
    channel_id: &str,
    adapter: &str,
    mode: ResponseMode,
    channel_model: &str,
    branch_model: &str,
    now_line: &str,
    home: Option<&crate::settings::HomeChannel>,
    pause_reason: Option<&str>,
) -> String {
    // A pause overrides everything below it, so it leads.
    let paused_line = match pause_reason {
        Some("") => "- paused: yes\n".to_string(),
        Some(reason) => format!("- paused: yes ({reason})\n"),
        None => String::new(),
    };
    format!(
        "status\n\
         {paused_line}\
         - agent: {agent_id}\n\
         - channel: {channel_id}\n\
         - adapter: {adapter}\n\
         - mode: {}\n\
         - channel model: {channel_model}\n\
         - branch model: {branch_model}\n\
         - home: {}\n\
         - time: {now_line}",
        mode_label(mode),
        home_label(home)
    )
}

/// How `/status` renders the home channel. An implicit home is marked so the
/// destination is never a silent default the user discovers by receiving
/// something unexpected.
fn home_label(home: Option<&crate::settings::HomeChannel>) -> String {
    match home {
        Some(home) if home.explicit => home.target.clone(),
        Some(home) => format!("{} (adopted on first run)", home.target),
        None => "not set".to_string(),
    }
}

/// Everything a control command needs, owned so execution can run in a
/// spawned task without borrowing the router loop.
pub struct ControlPlane {
    pub deps: crate::AgentDeps,
    pub conversation_id: String,
    /// Runtime adapter key of the surface the command arrived on.
    pub adapter: String,
    /// Settings defaults from the matched binding, if any.
    pub binding_settings: Option<crate::conversation::ConversationSettings>,
    pub surface: Surface,
    pub is_authority: bool,
    pub is_portal: bool,
}

impl ControlPlane {
    /// Execute a control action and return the reply text. `args` carries the
    /// validated argument string for actions that take one.
    pub async fn execute(&self, action: ControlAction, args: &str) -> String {
        match action {
            ControlAction::Status => self.status().await,
            ControlAction::SetResponseMode(mode) => self.set_response_mode(mode).await,
            ControlAction::Help => {
                crate::commands::REGISTRY.help_text_for(Some(self.surface), self.is_authority)
            }
            ControlAction::AgentId => self.deps.agent_id.to_string(),
            ControlAction::SetHome => self.set_home().await,
            ControlAction::SetPause => set_pause(&self.deps, args),
            ControlAction::WhoAmI => whoami_text(self.is_authority, self.surface),
            ControlAction::AutonomyStatus => autonomy_status(&self.deps).await,
            ControlAction::AutonomyOn => set_autonomy_enabled(&self.deps, true).await,
            ControlAction::AutonomyOff => set_autonomy_enabled(&self.deps, false).await,
        }
    }

    async fn set_home(&self) -> String {
        set_home_channel(&self.deps, &self.conversation_id, self.is_portal).await
    }

    /// Resolve the conversation's settings the same way channel creation
    /// does: per-conversation DB override > binding defaults > defaults.
    async fn resolved_settings(&self) -> ResolvedConversationSettings {
        let db_settings = if self.is_portal {
            let store =
                crate::conversation::PortalConversationStore::new(self.deps.sqlite_pool.clone());
            match store.get(&self.deps.agent_id, &self.conversation_id).await {
                Ok(conversation) => conversation.and_then(|conversation| conversation.settings),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        conversation_id = %self.conversation_id,
                        "failed to load portal conversation settings for control command"
                    );
                    None
                }
            }
        } else {
            let store =
                crate::conversation::ChannelSettingsStore::new(self.deps.sqlite_pool.clone());
            match store.get(&self.deps.agent_id, &self.conversation_id).await {
                Ok(settings) => settings,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        conversation_id = %self.conversation_id,
                        "failed to load channel settings for control command"
                    );
                    None
                }
            }
        };
        ResolvedConversationSettings::resolve(
            db_settings.as_ref(),
            self.binding_settings.as_ref(),
            None,
        )
    }

    /// The live channel's control handle, when one is running for this
    /// conversation.
    async fn live_channel(&self) -> Option<crate::agent::channel::ChannelControlHandle> {
        let channel_id: crate::ChannelId = std::sync::Arc::from(self.conversation_id.as_str());
        self.deps
            .process_control_registry
            .channel_handle(&channel_id)
            .await
    }

    async fn status(&self) -> String {
        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let routing = self.deps.runtime_config.routing.load();
        let resolved = self.resolved_settings().await;
        let channel_model = resolved
            .resolve_model("channel")
            .unwrap_or_else(|| routing.resolve(ProcessType::Channel, None));
        let branch_model = resolved
            .resolve_model("branch")
            .unwrap_or_else(|| routing.resolve(ProcessType::Branch, None));
        // A live channel's in-memory mode wins over the store: mode changes
        // persist asynchronously, so the cell is ahead of the DB briefly.
        let mode = match self.live_channel().await {
            Some(handle) => handle.response_mode(),
            None => resolved.response_mode,
        };
        status_text(
            &self.deps.agent_id,
            &self.conversation_id,
            &self.adapter,
            mode,
            channel_model,
            branch_model,
            &temporal_context.current_time_line(),
            self.deps.settings().and_then(|s| s.home_channel()).as_ref(),
            self.deps.pause_reason().as_deref(),
        )
    }

    async fn set_response_mode(&self, mode: ResponseMode) -> String {
        // On persistence failure the change is abandoned — no live update,
        // and the reply reports the failure instead of confirming a mode
        // that never persisted.
        if let Err(error) = persist_response_mode(
            &self.deps.sqlite_pool,
            &self.deps.agent_id,
            &self.conversation_id,
            self.is_portal,
            mode,
        )
        .await
        {
            tracing::warn!(
                %error,
                conversation_id = %self.conversation_id,
                ?mode,
                "failed to persist response mode"
            );
            return format!(
                "couldn't switch to {} mode — settings persistence failed, mode is unchanged",
                mode_name(mode)
            );
        }

        // Poke the live channel so the change applies mid-turn, not on the
        // next channel restart.
        if let Some(handle) = self.live_channel().await {
            handle.set_response_mode_live(mode);
        }

        mode_confirmation(mode).to_string()
    }
}

/// `/autonomy` reply built from the same status snapshot as the HTTP API.
pub async fn autonomy_status(deps: &crate::AgentDeps) -> String {
    let ceiling = **deps.autonomy_ceiling.load();
    match crate::api::autonomy::build_status(&deps.agent_id, deps, ceiling).await {
        Ok(status) => {
            let epoch = if status.current_run.is_some() {
                "running"
            } else {
                "idle"
            };
            let next = status.next_run_at.as_deref().unwrap_or("none");
            format!(
                "autonomy\n- configured: {}\n- effective: {}\n- ceiling: {}\n- epoch: {epoch}\n- next run: {next}",
                status.level, status.effective_level, ceiling
            )
        }
        Err(error) => {
            tracing::warn!(%error, agent = %deps.agent_id, "failed to read autonomy status");
            "couldn't read autonomy status".to_string()
        }
    }
}

/// Toggle future autonomy epochs while preserving the last enabled level.
pub async fn set_autonomy_enabled(deps: &crate::AgentDeps, enabled: bool) -> String {
    let Some(api_state) = deps.api_state.as_ref() else {
        return "autonomy settings aren't available — level unchanged".to_string();
    };
    let Some(settings) = deps.settings() else {
        return "settings storage isn't available — autonomy unchanged".to_string();
    };

    let result = crate::api::config::mutate_agent_autonomy_level(
        api_state,
        &deps.agent_id,
        move |current| {
            if enabled {
                if current != crate::config::AutonomyLevel::Off {
                    return Ok(crate::api::config::AutonomyLevelMutation::Unchanged);
                }
                let resume_level = settings.autonomy_resume_level().map_err(|error| {
                    tracing::warn!(%error, "failed to read autonomy resume level");
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                })?;
                Ok(crate::api::config::AutonomyLevelMutation::Set(
                    autonomy_toggle_target(current, true, resume_level),
                ))
            } else {
                if current == crate::config::AutonomyLevel::Off {
                    return Ok(crate::api::config::AutonomyLevelMutation::Unchanged);
                }
                Ok(crate::api::config::AutonomyLevelMutation::Set(
                    crate::config::AutonomyLevel::Off,
                ))
            }
        },
    )
    .await;

    let change = match result {
        Ok(change) => change,
        Err(error) => {
            tracing::warn!(status = %error, agent = %deps.agent_id, "failed to persist autonomy toggle");
            return "couldn't save autonomy setting — level unchanged".to_string();
        }
    };

    if change.previous != change.configured {
        deps.autonomy_control.request_check();
    }
    if enabled {
        if change.previous != crate::config::AutonomyLevel::Off {
            return format!(
                "autonomy is already enabled at {} (effective: {}).",
                change.configured, change.effective
            );
        }
        if change.configured == change.effective {
            format!("autonomy enabled at {}.", change.configured)
        } else {
            format!(
                "autonomy enabled at {}; the instance ceiling limits it to {}.",
                change.configured, change.effective
            )
        }
    } else if change.previous == crate::config::AutonomyLevel::Off {
        "autonomy is already off.".to_string()
    } else {
        format!(
            "autonomy off. new epochs won't start; current autonomy work can finish. /autonomy-on restores {}.",
            change.previous
        )
    }
}

fn autonomy_toggle_target(
    current: crate::config::AutonomyLevel,
    enabled: bool,
    resume_level: Option<crate::config::AutonomyLevel>,
) -> crate::config::AutonomyLevel {
    if !enabled {
        return crate::config::AutonomyLevel::Off;
    }
    if current != crate::config::AutonomyLevel::Off {
        return current;
    }
    resume_level.unwrap_or(crate::config::AutonomyLevel::Observe)
}

/// Resolve a conversation into the canonical delivery target that reaches it.
/// Portal conversations have no broadcast address, so they cannot be a home.
async fn conversation_broadcast_target(
    deps: &crate::AgentDeps,
    conversation_id: &str,
    is_portal: bool,
) -> std::result::Result<crate::messaging::target::BroadcastTarget, String> {
    if is_portal {
        return Err(
            "the portal isn't a delivery target — set the home from the chat that should \
             receive proactive messages"
                .to_string(),
        );
    }

    let store = crate::conversation::ChannelStore::new(deps.sqlite_pool.clone());
    let channel = match store.get(conversation_id).await {
        Ok(Some(channel)) => channel,
        Ok(None) => return Err("couldn't resolve this chat's delivery address".to_string()),
        Err(error) => {
            tracing::warn!(
                %error,
                %conversation_id,
                "failed to load channel while setting home"
            );
            return Err("couldn't read this chat's delivery address".to_string());
        }
    };

    crate::messaging::target::resolve_broadcast_target(&channel)
        .ok_or_else(|| "couldn't resolve this chat's delivery address".to_string())
}

/// `/whoami` reply: what the sender may do here, not who they are. The point
/// is to make the authority split legible before someone hits a denial.
pub fn whoami_text(is_authority: bool, surface: Surface) -> String {
    let restricted = crate::commands::REGISTRY
        .defs()
        .iter()
        .filter(|def| def.availability.on(surface))
        .filter(|def| def.access == crate::commands::CommandAccess::Authority)
        .count();

    if is_authority {
        format!(
            "you have authority in this chat — every command is available to you, \
             including the {restricted} that change my state."
        )
    } else {
        format!(
            "you can talk to me and run the read-only commands here. the {restricted} commands \
             that change my state are limited to this chat's authority list. /help lists what \
             you can run."
        )
    }
}

/// `/pause` reply, and the state change behind it. Empty args pause without a
/// stated reason; `off` resumes.
pub fn set_pause(deps: &crate::AgentDeps, args: &str) -> String {
    let Some(settings) = deps.runtime_config.settings.load().as_ref().clone() else {
        return "settings storage isn't available — pause state unchanged".to_string();
    };

    let args = args.trim();
    if args.eq_ignore_ascii_case("off") {
        if settings.pause_reason().is_none() {
            return "not paused.".to_string();
        }
        return match settings.set_paused(None) {
            Ok(()) => {
                tracing::info!(agent = %deps.agent_id, "resumed after pause");
                "resumed. new work starts again.".to_string()
            }
            Err(error) => {
                tracing::warn!(%error, "failed to clear pause");
                "couldn't clear the pause — still paused".to_string()
            }
        };
    }

    match settings.set_paused(Some(args)) {
        Ok(()) => {
            tracing::warn!(agent = %deps.agent_id, reason = args, "paused: new work will not start");
            let suffix = if args.is_empty() {
                String::new()
            } else {
                format!(" ({args})")
            };
            format!(
                "paused{suffix}. i won't start new work — commands still reach me. \
                 /pause off resumes."
            )
        }
        Err(error) => {
            tracing::warn!(%error, "failed to persist pause");
            "couldn't save the pause — still running".to_string()
        }
    }
}

/// Claim a conversation as the home channel on behalf of first-run adoption,
/// returning the canonical target when this conversation actually took it.
///
/// Unlike [`set_home_channel`] this never displaces an existing home, and it
/// is silent about every reason it might decline — the caller only cares
/// whether it now owns the home and therefore owes the user an announcement.
pub(crate) async fn adopt_home_channel(
    deps: &crate::AgentDeps,
    conversation_id: &str,
    is_portal: bool,
) -> Option<String> {
    let settings = deps.runtime_config.settings.load().as_ref().clone()?;
    if settings.home_channel().is_some() {
        return None;
    }

    let target = conversation_broadcast_target(deps, conversation_id, is_portal)
        .await
        .ok()?
        .to_string();

    match settings.adopt_home_channel(&target) {
        Ok(true) => {
            tracing::info!(
                agent = %deps.agent_id,
                home_channel = %target,
                "adopted home channel on first completed turn"
            );
            Some(target)
        }
        Ok(false) => None,
        Err(error) => {
            tracing::warn!(%error, "failed to adopt home channel");
            None
        }
    }
}

/// Set a conversation as the instance's home channel, returning the reply.
///
/// Shared by `/sethome` and the `set_home_channel` tool so both entry points
/// validate and persist identically. Validation runs here rather than at first
/// send, so a home the agent cannot reach is rejected while the user is still
/// looking at the reply.
pub async fn set_home_channel(
    deps: &crate::AgentDeps,
    conversation_id: &str,
    is_portal: bool,
) -> String {
    let target = match conversation_broadcast_target(deps, conversation_id, is_portal).await {
        Ok(target) => target,
        Err(message) => return message,
    };

    if let Some(manager) = deps.messaging_manager.as_ref()
        && !manager.has_adapter(&target.adapter).await
    {
        return format!("no '{}' adapter is running — home not set", target.adapter);
    }

    let Some(settings) = deps.runtime_config.settings.load().as_ref().clone() else {
        return "settings storage isn't available — home not set".to_string();
    };

    let canonical = target.to_string();
    match settings.set_home_channel(&canonical) {
        Ok(()) => {
            tracing::info!(
                agent = %deps.agent_id,
                home_channel = %canonical,
                "home channel set"
            );
            format!("home channel set to this chat ({canonical}). proactive messages land here.")
        }
        Err(error) => {
            tracing::warn!(%error, "failed to persist home channel");
            "couldn't save the home channel — it's unchanged".to_string()
        }
    }
}

/// Persist the response mode through the stores' atomic field updates,
/// leaving every other settings field untouched. No settings read happens
/// here, so concurrent whole-row writers can't be clobbered with stale
/// fields, and an unreadable record can't be replaced with defaults.
pub(crate) async fn persist_response_mode(
    pool: &sqlx::SqlitePool,
    agent_id: &str,
    conversation_id: &str,
    is_portal: bool,
    mode: ResponseMode,
) -> anyhow::Result<()> {
    if is_portal {
        let store = crate::conversation::PortalConversationStore::new(pool.clone());
        let updated = store
            .set_response_mode(agent_id, conversation_id, mode)
            .await?;
        if !updated {
            anyhow::bail!("portal conversation {conversation_id} not found");
        }
    } else {
        let store = crate::conversation::ChannelSettingsStore::new(pool.clone());
        store
            .set_response_mode(agent_id, conversation_id, mode)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn status_with(
        home: Option<&crate::settings::HomeChannel>,
        pause_reason: Option<&str>,
    ) -> String {
        status_text(
            "orion",
            "discord:guild:1",
            "discord",
            ResponseMode::Active,
            "model-a",
            "model-b",
            "now",
            home,
            pause_reason,
        )
    }

    fn status_with_home(home: Option<&crate::settings::HomeChannel>) -> String {
        status_with(home, None)
    }

    #[test]
    fn status_omits_the_pause_line_when_running() {
        assert!(!status_with(None, None).contains("paused"));
    }

    #[test]
    fn status_leads_with_a_pause_and_its_reason() {
        let body = status_with(None, Some("deploying"));
        assert!(body.contains("- paused: yes (deploying)"));
        // The pause outranks everything it suppresses, so it reads first.
        assert!(body.find("paused").unwrap() < body.find("- agent:").unwrap());
    }

    #[test]
    fn status_reports_a_reasonless_pause_without_empty_parens() {
        let body = status_with(None, Some(""));
        assert!(body.contains("- paused: yes\n"));
        assert!(!body.contains("()"));
    }

    #[test]
    fn whoami_distinguishes_authority_from_everyone() {
        let authority = whoami_text(true, Surface::Discord);
        let everyone = whoami_text(false, Surface::Discord);
        assert!(authority.contains("you have authority"));
        assert!(everyone.contains("read-only"));
        // Both name the same count, so the two replies describe one boundary.
        let restricted = crate::commands::REGISTRY
            .defs()
            .iter()
            .filter(|def| def.availability.on(Surface::Discord))
            .filter(|def| def.access == crate::commands::CommandAccess::Authority)
            .count();
        assert!(authority.contains(&restricted.to_string()));
        assert!(everyone.contains(&restricted.to_string()));
    }

    #[test]
    fn autonomy_enable_restores_the_last_level_or_observe() {
        use crate::config::AutonomyLevel;

        assert_eq!(
            autonomy_toggle_target(AutonomyLevel::Off, true, None),
            AutonomyLevel::Observe
        );
        for level in [
            AutonomyLevel::Observe,
            AutonomyLevel::Suggest,
            AutonomyLevel::Act,
        ] {
            assert_eq!(
                autonomy_toggle_target(AutonomyLevel::Off, true, Some(level)),
                level
            );
            assert_eq!(autonomy_toggle_target(level, true, None), level);
            assert_eq!(
                autonomy_toggle_target(level, false, Some(level)),
                AutonomyLevel::Off
            );
        }
    }

    #[test]
    fn status_reports_an_unset_home() {
        assert!(status_with_home(None).contains("- home: not set"));
    }

    #[test]
    fn status_marks_an_adopted_home_as_implicit() {
        let home = crate::settings::HomeChannel {
            target: "discord:123".to_string(),
            explicit: false,
        };
        assert!(
            status_with_home(Some(&home)).contains("- home: discord:123 (adopted on first run)")
        );
    }

    #[test]
    fn status_reports_an_explicit_home_bare() {
        let home = crate::settings::HomeChannel {
            target: "discord:123".to_string(),
            explicit: true,
        };
        assert!(status_with_home(Some(&home)).contains("- home: discord:123\n"));
    }

    async fn memory_pool() -> sqlx::SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect")
    }

    async fn create_channel_settings_table(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "CREATE TABLE channel_settings (
                agent_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                settings TEXT NOT NULL DEFAULT '{}',
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (agent_id, conversation_id)
            )",
        )
        .execute(pool)
        .await
        .expect("channel_settings table should create");
    }

    async fn create_portal_conversations_table(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "CREATE TABLE portal_conversations (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                title TEXT NOT NULL,
                title_source TEXT NOT NULL DEFAULT 'system',
                archived INTEGER NOT NULL DEFAULT 0,
                settings TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(pool)
        .await
        .expect("portal_conversations table should create");
    }

    #[tokio::test]
    async fn channel_mode_change_preserves_other_settings() {
        let pool = memory_pool().await;
        create_channel_settings_table(&pool).await;

        let store = crate::conversation::ChannelSettingsStore::new(pool.clone());
        let existing = crate::conversation::ConversationSettings {
            model: Some("special-model".into()),
            ..Default::default()
        };
        store.upsert("agent", "conv", &existing).await.unwrap();

        persist_response_mode(&pool, "agent", "conv", false, ResponseMode::Observe)
            .await
            .unwrap();

        let loaded = store.get("agent", "conv").await.unwrap().unwrap();
        assert_eq!(loaded.response_mode, ResponseMode::Observe);
        assert_eq!(
            loaded.model.as_deref(),
            Some("special-model"),
            "a mode change must not clobber other persisted settings"
        );
    }

    #[tokio::test]
    async fn channel_store_failure_propagates_instead_of_writing_defaults() {
        // No channel_settings table: the settings read fails, and the
        // failure must surface instead of turning into a default upsert
        // and a false confirmation.
        let pool = memory_pool().await;
        let result =
            persist_response_mode(&pool, "agent", "conv", false, ResponseMode::Observe).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn portal_mode_change_preserves_other_settings() {
        let pool = memory_pool().await;
        create_portal_conversations_table(&pool).await;

        let store = crate::conversation::PortalConversationStore::new(pool.clone());
        store.ensure("agent", "session").await.unwrap();
        let existing = crate::conversation::ConversationSettings {
            model: Some("special-model".into()),
            ..Default::default()
        };
        store
            .update("agent", "session", None, None, Some(existing))
            .await
            .unwrap();

        persist_response_mode(&pool, "agent", "session", true, ResponseMode::MentionOnly)
            .await
            .unwrap();

        let loaded = store
            .get("agent", "session")
            .await
            .unwrap()
            .unwrap()
            .settings
            .unwrap();
        assert_eq!(loaded.response_mode, ResponseMode::MentionOnly);
        assert_eq!(loaded.model.as_deref(), Some("special-model"));
    }

    #[tokio::test]
    async fn portal_mode_change_initializes_null_settings() {
        let pool = memory_pool().await;
        create_portal_conversations_table(&pool).await;

        let store = crate::conversation::PortalConversationStore::new(pool.clone());
        store.ensure("agent", "session").await.unwrap();

        persist_response_mode(&pool, "agent", "session", true, ResponseMode::Observe)
            .await
            .unwrap();

        let loaded = store
            .get("agent", "session")
            .await
            .unwrap()
            .unwrap()
            .settings
            .unwrap();
        assert_eq!(loaded.response_mode, ResponseMode::Observe);
    }

    #[tokio::test]
    async fn channel_mode_change_handles_empty_string_settings() {
        let pool = memory_pool().await;
        create_channel_settings_table(&pool).await;

        sqlx::query(
            "INSERT INTO channel_settings (agent_id, conversation_id, settings) VALUES (?, ?, '')",
        )
        .bind("agent")
        .bind("conv")
        .execute(&pool)
        .await
        .unwrap();

        persist_response_mode(&pool, "agent", "conv", false, ResponseMode::MentionOnly)
            .await
            .unwrap();

        let store = crate::conversation::ChannelSettingsStore::new(pool.clone());
        let loaded = store.get("agent", "conv").await.unwrap().unwrap();
        assert_eq!(loaded.response_mode, ResponseMode::MentionOnly);
    }

    #[tokio::test]
    async fn portal_missing_conversation_fails_without_writing() {
        let pool = memory_pool().await;
        create_portal_conversations_table(&pool).await;

        let result =
            persist_response_mode(&pool, "agent", "missing", true, ResponseMode::Observe).await;
        assert!(result.is_err());

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM portal_conversations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "a failed mode change must not create records");
    }
}
