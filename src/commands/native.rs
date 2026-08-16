//! Platform-neutral specs for native command registration.
//!
//! Adapters map these to their platform types (serenity `CreateCommand`,
//! teloxide `BotCommand`, Slack subcommand listings); generating the specs
//! here keeps the registry the single source of truth and gives the parity
//! test one surface to walk per platform.

use super::registry::{ArgSpec, CommandDef, REGISTRY, Surface};

/// Discord's application-command cap. Registration is cap-aware: commands
/// register in table order and overflow is dropped with one log line.
pub const DISCORD_COMMAND_CAP: usize = 100;

/// Telegram menu limits (Bot API): command names 1–32 chars of
/// `[a-z0-9_]`, descriptions 3–256 chars, at most 100 entries.
pub const TELEGRAM_COMMAND_CAP: usize = 100;
const TELEGRAM_NAME_MAX: usize = 32;
const TELEGRAM_DESCRIPTION_MAX: usize = 256;

/// A native command argument, mapped from [`ArgSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArg {
    /// Free-text option; `required` distinguishes `Required` from `Optional`.
    Text { hint: String, required: bool },
    /// Closed choice set.
    Choice { options: Vec<String> },
}

/// A platform-neutral command spec for native registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCommandSpec {
    pub name: String,
    pub description: String,
    pub arg: Option<NativeArg>,
}

fn spec_for(def: &CommandDef) -> NativeCommandSpec {
    let arg = match def.args {
        ArgSpec::None => None,
        ArgSpec::Optional(hint) => Some(NativeArg::Text {
            hint: hint.to_string(),
            required: false,
        }),
        ArgSpec::Required(hint) => Some(NativeArg::Text {
            hint: hint.to_string(),
            required: true,
        }),
        ArgSpec::Choice(options) => Some(NativeArg::Choice {
            options: options.iter().map(|option| option.to_string()).collect(),
        }),
    };
    NativeCommandSpec {
        name: def.name.to_string(),
        description: def.description.to_string(),
        arg,
    }
}

/// Command specs to register as Discord application commands, in table
/// order, truncated to the cap. Returns the specs and the number dropped.
pub fn discord_commands() -> (Vec<NativeCommandSpec>, usize) {
    let available: Vec<NativeCommandSpec> = REGISTRY
        .defs()
        .iter()
        .filter(|def| def.availability.on(Surface::Discord))
        .map(spec_for)
        .collect();
    let dropped = available.len().saturating_sub(DISCORD_COMMAND_CAP);
    let mut specs = available;
    specs.truncate(DISCORD_COMMAND_CAP);
    (specs, dropped)
}

/// A Telegram menu entry. `command` is the mangled form (hyphens become
/// underscores — Telegram rejects hyphens); the parser folds underscores
/// back to hyphens on resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramMenuEntry {
    pub command: String,
    pub description: String,
}

/// The Telegram bot menu, generated from the registry. Choice values are
/// appended to the description as hints; entries that cannot be expressed
/// within Telegram's naming rules are skipped (none exist today — the
/// parity test enforces that skips stay deliberate).
pub fn telegram_menu() -> Vec<TelegramMenuEntry> {
    REGISTRY
        .defs()
        .iter()
        .filter(|def| def.availability.on(Surface::Telegram))
        .filter_map(|def| {
            let command = def.name.replace('-', "_").to_ascii_lowercase();
            let valid = !command.is_empty()
                && command.len() <= TELEGRAM_NAME_MAX
                && command
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !valid {
                tracing::warn!(
                    command = def.name,
                    "command name cannot be registered in the telegram menu, skipping"
                );
                return None;
            }
            let mut description = def.description.to_string();
            if let Some(hint) = def.args.hint() {
                description.push_str(&format!(" {hint}"));
            }
            let mut description: String =
                description.chars().take(TELEGRAM_DESCRIPTION_MAX).collect();
            if description.chars().count() < 3 {
                description = format!("/{command}");
            }
            Some(TelegramMenuEntry {
                command,
                description,
            })
        })
        .take(TELEGRAM_COMMAND_CAP)
        .collect()
}

/// Slack `/spacebot` subcommands: every command available on Slack. The
/// umbrella command's help/usage listing derives from this.
pub fn slack_subcommands() -> Vec<NativeCommandSpec> {
    REGISTRY
        .defs()
        .iter()
        .filter(|def| def.availability.on(Surface::Slack))
        .map(spec_for)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::registry::{CommandHandler, Surface};

    #[test]
    fn discord_specs_cover_every_available_command_within_cap() {
        let (specs, dropped) = discord_commands();
        assert_eq!(dropped, 0, "registry exceeds the discord cap");
        let expected: Vec<&str> = REGISTRY
            .defs()
            .iter()
            .filter(|def| def.availability.on(Surface::Discord))
            .map(|def| def.name)
            .collect();
        let actual: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn telegram_menu_covers_every_available_command() {
        let menu = telegram_menu();
        let expected: Vec<String> = REGISTRY
            .defs()
            .iter()
            .filter(|def| def.availability.on(Surface::Telegram))
            .map(|def| def.name.replace('-', "_"))
            .collect();
        let actual: Vec<&str> = menu.iter().map(|entry| entry.command.as_str()).collect();
        assert_eq!(actual, expected, "telegram menu drops a command silently");
    }

    #[test]
    fn telegram_menu_entries_satisfy_platform_limits() {
        for entry in telegram_menu() {
            assert!(
                (1..=32).contains(&entry.command.len()),
                "telegram command name length: {}",
                entry.command
            );
            assert!(
                entry
                    .command
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "invalid telegram command name: {}",
                entry.command
            );
            assert!(
                (3..=256).contains(&entry.description.chars().count()),
                "telegram description length for /{}",
                entry.command
            );
        }
    }

    #[test]
    fn mangled_telegram_names_resolve_to_canonical_defs() {
        for entry in telegram_menu() {
            let def = REGISTRY
                .resolve(&entry.command)
                .unwrap_or_else(|| panic!("/{} does not resolve back", entry.command));
            assert_eq!(def.name.replace('-', "_"), entry.command);
        }
    }

    #[test]
    fn every_command_is_exposed_on_every_available_surface() {
        // Parity walk: a command available on a platform must be reachable
        // through that platform's native surface (or the shared text
        // parser for text adapters and portal, which accept everything).
        // A command missing from a native listing needs an explicit
        // availability opt-out in the registry, not a silent drop.
        let (discord, _) = discord_commands();
        let telegram = telegram_menu();
        let slack = slack_subcommands();
        for def in REGISTRY.defs() {
            if def.availability.on(Surface::Discord) {
                assert!(
                    discord.iter().any(|spec| spec.name == def.name),
                    "/{} available on discord but not natively registered",
                    def.name
                );
            }
            if def.availability.on(Surface::Telegram) {
                assert!(
                    telegram
                        .iter()
                        .any(|entry| entry.command == def.name.replace('-', "_")),
                    "/{} available on telegram but missing from the menu",
                    def.name
                );
            }
            if def.availability.on(Surface::Slack) {
                assert!(
                    slack.iter().any(|spec| spec.name == def.name),
                    "/{} available on slack but missing from /spacebot subcommands",
                    def.name
                );
            }
        }
    }

    #[test]
    fn control_and_agent_commands_both_appear_in_native_listings() {
        let (discord, _) = discord_commands();
        let has_control = discord.iter().any(|spec| {
            matches!(
                REGISTRY.resolve(&spec.name).unwrap().handler,
                CommandHandler::Control(_)
            )
        });
        let has_agent = discord.iter().any(|spec| {
            matches!(
                REGISTRY.resolve(&spec.name).unwrap().handler,
                CommandHandler::Agent(_)
            )
        });
        assert!(has_control && has_agent);
    }

    #[test]
    fn autonomy_controls_are_exposed_on_native_surfaces() {
        let expected = ["autonomy", "autonomy-on", "autonomy-off"];
        let (discord, _) = discord_commands();
        let telegram = telegram_menu();
        let slack = slack_subcommands();

        for command in expected {
            assert!(discord.iter().any(|spec| spec.name == command));
            assert!(
                telegram
                    .iter()
                    .any(|entry| entry.command == command.replace('-', "_"))
            );
            assert!(slack.iter().any(|spec| spec.name == command));
        }
    }
}
