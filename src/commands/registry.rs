//! Typed slash-command registry.
//!
//! Every command is defined once in [`COMMANDS`]. Parsing, dispatch, and
//! `/help` all derive from the same table, so adding a command is one entry
//! here and a handler arm — nothing else to keep in sync. See
//! `docs/design-docs/slash-commands.md` for the full design.

use crate::conversation::settings::ResponseMode;

/// Definition of a single slash command.
#[derive(Debug)]
pub struct CommandDef {
    /// Canonical name without the slash (e.g. "status").
    pub name: &'static str,
    /// Short description shown in `/help` and platform menus.
    pub description: &'static str,
    /// Grouping for `/help` display.
    pub category: CommandCategory,
    /// Alternative names that resolve to this command.
    pub aliases: &'static [&'static str],
    /// Argument shape — drives validation and help hints.
    pub args: ArgSpec,
    /// How this command executes.
    pub handler: CommandHandler,
    /// Who may run this command in a scope with an authority list configured.
    pub access: CommandAccess,
    /// What happens when an Agent command arrives while a turn is in flight.
    /// Control commands are busy-immune by construction; this field is only
    /// consulted for `CommandHandler::Agent`.
    pub busy: BusyPolicy,
    /// Which platform surfaces expose this command.
    pub availability: CommandAvailability,
}

/// Who may run a command. Layered on top of admission (bindings and adapter
/// permission snapshots): access never widens who may talk to the agent, it
/// only restricts who may change state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAccess {
    Everyone,
    /// Requires the sender to be in the authority list for this scope. With
    /// no authority list configured the command is open to everyone the
    /// binding already admits.
    Authority,
}

/// What happens when an Agent command arrives while a turn is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyPolicy {
    /// Wait for the current turn, then run as a normal turn.
    Queue,
    /// Refuse mid-turn with a pointer to /stop.
    Reject,
}

/// Which platform surfaces expose a command. Used by native registration
/// (Discord application commands, Telegram menu, Slack subcommands) and the
/// parity test; the shared text parser accepts every command everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandAvailability {
    pub portal: bool,
    pub discord: bool,
    pub slack: bool,
    pub telegram: bool,
    /// Signal, Mattermost, Twitch, Email, Webhook.
    pub text_adapters: bool,
}

impl CommandAvailability {
    pub const ALL: CommandAvailability = CommandAvailability {
        portal: true,
        discord: true,
        slack: true,
        telegram: true,
        text_adapters: true,
    };

    pub fn on(&self, surface: Surface) -> bool {
        match surface {
            Surface::Portal => self.portal,
            Surface::Discord => self.discord,
            Surface::Slack => self.slack,
            Surface::Telegram => self.telegram,
            Surface::TextAdapters => self.text_adapters,
        }
    }
}

/// A platform surface, for availability checks. Maps from
/// `InboundMessage.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Portal,
    Discord,
    Slack,
    Telegram,
    TextAdapters,
}

impl Surface {
    pub fn from_source(source: &str) -> Surface {
        match source {
            "portal" => Surface::Portal,
            "discord" => Surface::Discord,
            "slack" => Surface::Slack,
            "telegram" => Surface::Telegram,
            _ => Surface::TextAdapters,
        }
    }
}

/// Grouping for `/help` display, rendered in [`CATEGORY_ORDER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Session,
    Response,
    Memory,
    Tasks,
    Skills,
    Info,
    Config,
}

/// Argument shape for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgSpec {
    /// Takes no arguments. Trailing text means the message is conversation
    /// that happens to start with a command word ("/status update: shipped")
    /// and must reach the model untouched.
    None,
    /// Optional free text, named for the help hint: "[query]".
    Optional(&'static str),
    /// Required free text: "<prompt>". Missing args produce a usage reply.
    Required(&'static str),
    /// Closed set, validated case-insensitively: "[on|off|status]".
    /// Empty args are allowed (commands treat that as a status query).
    Choice(&'static [&'static str]),
}

impl ArgSpec {
    /// Help hint for this argument shape, e.g. "[query]" or "[on|off]".
    pub fn hint(&self) -> Option<String> {
        match self {
            ArgSpec::None => None,
            ArgSpec::Optional(hint) | ArgSpec::Required(hint) => Some((*hint).to_string()),
            ArgSpec::Choice(options) => Some(format!("[{}]", options.join("|"))),
        }
    }
}

/// How a command executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandHandler {
    /// Executes deterministically on the channel control plane — settings
    /// store, process control, runtime config. Never consumes an agent turn.
    Control(ControlAction),
    /// Forwarded to the agent as a normal turn.
    Agent(AgentAction),
}

/// Control-plane operations. The channel executes these directly; the
/// registry stays free of channel internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Report mode, resolved models, adapter, and current time.
    Status,
    /// Persist a new response mode for this channel.
    SetResponseMode(ResponseMode),
    /// Render the generated command list.
    Help,
    /// Print the runtime agent id. Deterministic so identity checks bypass
    /// model output drift.
    AgentId,
    /// Adopt the calling conversation as the instance's home channel.
    SetHome,
    /// Hold off on starting new work, or resume.
    SetPause,
    /// Report the sender's authority in this scope.
    WhoAmI,
    /// Report the configured and effective autonomy levels.
    AutonomyStatus,
    /// Restore the most recently enabled autonomy level.
    AutonomyOn,
    /// Disable future autonomy epochs while current work settles.
    AutonomyOff,
}

/// Agent-turn commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAction {
    /// Replace the raw text with the named prompt template, rendered as a
    /// normal agent turn. The value is a template key in the prompt engine
    /// (e.g. "commands/tasks").
    PromptTemplate(&'static str),
}

/// A successfully parsed command with validated, normalized arguments.
#[derive(Debug)]
pub struct ParsedCommand {
    pub def: &'static CommandDef,
    pub args: String,
}

/// Result of [`CommandRegistry::parse`].
#[derive(Debug)]
pub enum ParseResult {
    /// Recognized command with valid arguments.
    Command(ParsedCommand),
    /// Recognized command with invalid arguments — reply with usage text,
    /// do not dispatch.
    Usage(&'static CommandDef, String),
    /// Not a command; the text flows to the model untouched.
    NotACommand,
}

/// Lookup and parsing over a static command table.
pub struct CommandRegistry {
    defs: &'static [CommandDef],
}

/// Category display order and labels for `/help`.
const CATEGORY_ORDER: &[(CommandCategory, &str)] = &[
    (CommandCategory::Session, "session"),
    (CommandCategory::Response, "response mode"),
    (CommandCategory::Memory, "memory"),
    (CommandCategory::Tasks, "tasks"),
    (CommandCategory::Skills, "skills"),
    (CommandCategory::Info, "info"),
    (CommandCategory::Config, "config"),
];

impl CommandRegistry {
    pub const fn new(defs: &'static [CommandDef]) -> Self {
        Self { defs }
    }

    /// Resolve a bare command token (no slash) across names and aliases,
    /// case-insensitively. Underscores fold to hyphens so Telegram's menu
    /// form of a hyphenated name (`/mention_only` — Telegram rejects hyphens
    /// in registered commands) resolves to the canonical def.
    pub fn resolve(&self, name: &str) -> Option<&'static CommandDef> {
        let folded = |candidate: &str| {
            candidate.len() == name.len()
                && candidate
                    .chars()
                    .zip(name.chars())
                    .all(|(a, b)| a.eq_ignore_ascii_case(&b) || (a == '-' && b == '_'))
        };
        self.defs
            .iter()
            .find(|def| folded(def.name) || def.aliases.iter().any(|alias| folded(alias)))
    }

    /// Parse message text into a command.
    ///
    /// Normalization: strips a Telegram-style `@botname` suffix from the
    /// command token, rejects tokens containing `/` so pasted file paths
    /// never parse as commands, and un-mangles smart dashes in arguments
    /// (iOS autocorrects `--` to `—` and `-` to `–`).
    pub fn parse(&self, text: &str) -> ParseResult {
        self.parse_addressed(text, None)
    }

    /// [`parse`](Self::parse) with the receiving bot's username, when the
    /// surface knows it. A `/cmd@other_bot` addressed to a different bot in a
    /// Telegram group is conversation, not a command for us.
    pub fn parse_addressed(&self, text: &str, bot_username: Option<&str>) -> ParseResult {
        let trimmed = text.trim();
        let Some(body) = trimmed.strip_prefix('/') else {
            return ParseResult::NotACommand;
        };
        let (token, rest) = match body.split_once(char::is_whitespace) {
            Some((token, rest)) => (token, rest),
            None => (body, ""),
        };
        if token.is_empty() || token.contains('/') {
            return ParseResult::NotACommand;
        }
        let token = match token.split_once('@') {
            Some((bare, addressed)) => {
                if let Some(own_name) = bot_username
                    && !addressed.eq_ignore_ascii_case(own_name)
                {
                    return ParseResult::NotACommand;
                }
                bare
            }
            None => token,
        };
        let Some(def) = self.resolve(token) else {
            return ParseResult::NotACommand;
        };

        let args = normalize_smart_dashes(rest.trim());
        match def.args {
            ArgSpec::None => {
                if args.is_empty() {
                    ParseResult::Command(ParsedCommand { def, args })
                } else {
                    ParseResult::NotACommand
                }
            }
            ArgSpec::Optional(_) => ParseResult::Command(ParsedCommand { def, args }),
            ArgSpec::Required(hint) => {
                if args.is_empty() {
                    ParseResult::Usage(def, format!("usage: /{} {}", def.name, hint))
                } else {
                    ParseResult::Command(ParsedCommand { def, args })
                }
            }
            ArgSpec::Choice(options) => {
                if args.is_empty() || options.iter().any(|opt| opt.eq_ignore_ascii_case(&args)) {
                    ParseResult::Command(ParsedCommand {
                        def,
                        args: args.to_ascii_lowercase(),
                    })
                } else {
                    ParseResult::Usage(def, format!("usage: /{} [{}]", def.name, options.join("|")))
                }
            }
        }
    }

    /// Render the `/help` body: every command grouped by category, with
    /// aliases and argument hints. Generated so it cannot drift from the
    /// table.
    pub fn help_text(&self) -> String {
        self.help_text_for(None, true)
    }

    /// [`help_text`](Self::help_text) filtered to what the caller can see and
    /// run: commands available on `surface` (all surfaces when `None`), minus
    /// Authority commands when the caller lacks authority. A user never sees
    /// a command they can't run.
    pub fn help_text_for(&self, surface: Option<Surface>, is_authority: bool) -> String {
        let visible = |def: &&CommandDef| {
            surface.is_none_or(|surface| def.availability.on(surface))
                && (is_authority || def.access == CommandAccess::Everyone)
        };
        let mut out = String::from("commands:");
        for (category, label) in CATEGORY_ORDER {
            let defs: Vec<&CommandDef> = self
                .defs
                .iter()
                .filter(|def| def.category == *category)
                .filter(visible)
                .collect();
            if defs.is_empty() {
                continue;
            }
            out.push_str("\n\n");
            out.push_str(label);
            out.push(':');
            for def in defs {
                out.push_str("\n- /");
                out.push_str(def.name);
                if let Some(hint) = def.args.hint() {
                    out.push(' ');
                    out.push_str(&hint);
                }
                if !def.aliases.is_empty() {
                    let alias_list = def
                        .aliases
                        .iter()
                        .map(|alias| format!("/{alias}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!(" (or {alias_list})"));
                }
                out.push_str(": ");
                out.push_str(def.description);
            }
        }
        out
    }

    pub fn defs(&self) -> &'static [CommandDef] {
        self.defs
    }
}

/// iOS autocorrects `--` to an em dash and `-` to an en dash, silently
/// breaking flag-style arguments typed from a phone. The double-em case is
/// replaced first so a mangled `----` collapses correctly.
fn normalize_smart_dashes(args: &str) -> String {
    args.replace("\u{2014}\u{2014}", "--")
        .replace('\u{2014}', "--")
        .replace('\u{2013}', "-")
}

/// The command table. Order within a category is display order in `/help`.
pub static COMMANDS: &[CommandDef] = &[
    CommandDef {
        name: "sethome",
        description: "set this chat as the home channel",
        category: CommandCategory::Session,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::SetHome),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "pause",
        description: "stop starting new work everywhere; '/pause off' resumes",
        category: CommandCategory::Session,
        aliases: &[],
        args: ArgSpec::Optional("[reason | off]"),
        handler: CommandHandler::Control(ControlAction::SetPause),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "autonomy",
        description: "show configured and effective autonomy",
        category: CommandCategory::Config,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::AutonomyStatus),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "autonomy-on",
        description: "restore the last enabled autonomy level",
        category: CommandCategory::Config,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::AutonomyOn),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "autonomy-off",
        description: "stop new autonomy epochs; current work settles",
        category: CommandCategory::Config,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::AutonomyOff),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "active",
        description: "normal reply mode",
        category: CommandCategory::Response,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::SetResponseMode(ResponseMode::Active)),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "mention-only",
        description: "only respond when @mentioned, replied to, or given a command",
        category: CommandCategory::Response,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::SetResponseMode(ResponseMode::MentionOnly)),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "quiet",
        description: "learn from conversation, never respond",
        category: CommandCategory::Response,
        aliases: &["observe"],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::SetResponseMode(ResponseMode::Observe)),
        access: CommandAccess::Authority,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "tasks",
        description: "ready task list",
        category: CommandCategory::Tasks,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Agent(AgentAction::PromptTemplate("commands/tasks")),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "today",
        description: "in-progress + ready task snapshot",
        category: CommandCategory::Tasks,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Agent(AgentAction::PromptTemplate("commands/today")),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "digest",
        description: "one-shot day digest (00:00 -> now)",
        category: CommandCategory::Tasks,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Agent(AgentAction::PromptTemplate("commands/digest")),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "status",
        description: "current mode, models, binding snapshot",
        category: CommandCategory::Info,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::Status),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "help",
        description: "show available commands",
        category: CommandCategory::Info,
        aliases: &["commands"],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::Help),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "whoami",
        description: "show your command access in this chat",
        category: CommandCategory::Info,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::WhoAmI),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
    CommandDef {
        name: "agent-id",
        description: "runtime agent id",
        category: CommandCategory::Info,
        aliases: &[],
        args: ArgSpec::None,
        handler: CommandHandler::Control(ControlAction::AgentId),
        access: CommandAccess::Everyone,
        busy: BusyPolicy::Queue,
        availability: CommandAvailability::ALL,
    },
];

/// The instance-wide registry over [`COMMANDS`].
pub static REGISTRY: CommandRegistry = CommandRegistry::new(COMMANDS);

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(result: ParseResult) -> ParsedCommand {
        match result {
            ParseResult::Command(cmd) => cmd,
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn parses_basic_command() {
        let cmd = parsed(REGISTRY.parse("/status"));
        assert_eq!(cmd.def.name, "status");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn autonomy_commands_have_separate_read_and_write_authority() {
        let status = parsed(REGISTRY.parse("/autonomy"));
        let enable = parsed(REGISTRY.parse("/autonomy-on"));
        let disable = parsed(REGISTRY.parse("/autonomy-off"));

        assert_eq!(status.def.access, CommandAccess::Everyone);
        assert_eq!(enable.def.access, CommandAccess::Authority);
        assert_eq!(disable.def.access, CommandAccess::Authority);
        assert!(matches!(status.def.handler, CommandHandler::Control(_)));
        assert!(matches!(enable.def.handler, CommandHandler::Control(_)));
        assert!(matches!(disable.def.handler, CommandHandler::Control(_)));
    }

    #[test]
    fn parses_with_surrounding_whitespace() {
        let cmd = parsed(REGISTRY.parse("  /help  "));
        assert_eq!(cmd.def.name, "help");
    }

    #[test]
    fn resolves_aliases_to_canonical_def() {
        assert_eq!(parsed(REGISTRY.parse("/observe")).def.name, "quiet");
        assert_eq!(parsed(REGISTRY.parse("/commands")).def.name, "help");
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(parsed(REGISTRY.parse("/STATUS")).def.name, "status");
        assert_eq!(parsed(REGISTRY.parse("/Observe")).def.name, "quiet");
    }

    #[test]
    fn strips_telegram_botname_suffix() {
        let cmd = parsed(REGISTRY.parse("/status@spacebot_bot"));
        assert_eq!(cmd.def.name, "status");
    }

    #[test]
    fn file_paths_are_not_commands() {
        assert!(matches!(
            REGISTRY.parse("/usr/bin/env python"),
            ParseResult::NotACommand
        ));
        assert!(matches!(REGISTRY.parse("/"), ParseResult::NotACommand));
    }

    #[test]
    fn unknown_slash_words_are_not_commands() {
        assert!(matches!(
            REGISTRY.parse("/shrug whatever"),
            ParseResult::NotACommand
        ));
    }

    #[test]
    fn trailing_text_on_no_arg_command_is_conversation() {
        // "/status update: we shipped" is a sentence, not a command.
        assert!(matches!(
            REGISTRY.parse("/status update: we shipped"),
            ParseResult::NotACommand
        ));
    }

    static TEST_COMMANDS: &[CommandDef] = &[
        CommandDef {
            name: "memory",
            description: "search memories",
            category: CommandCategory::Memory,
            aliases: &[],
            args: ArgSpec::Optional("[query]"),
            handler: CommandHandler::Control(ControlAction::Help),
            access: CommandAccess::Everyone,
            busy: BusyPolicy::Queue,
            availability: CommandAvailability::ALL,
        },
        CommandDef {
            name: "background",
            description: "run in background",
            category: CommandCategory::Session,
            aliases: &[],
            args: ArgSpec::Required("<prompt>"),
            handler: CommandHandler::Control(ControlAction::Help),
            access: CommandAccess::Everyone,
            busy: BusyPolicy::Queue,
            availability: CommandAvailability::ALL,
        },
        CommandDef {
            name: "voice",
            description: "toggle voice",
            category: CommandCategory::Config,
            aliases: &[],
            args: ArgSpec::Choice(&["on", "off", "status"]),
            handler: CommandHandler::Control(ControlAction::Help),
            access: CommandAccess::Everyone,
            busy: BusyPolicy::Queue,
            availability: CommandAvailability::ALL,
        },
    ];

    static TEST_REGISTRY: CommandRegistry = CommandRegistry::new(TEST_COMMANDS);

    #[test]
    fn optional_args_pass_through() {
        let cmd = parsed(TEST_REGISTRY.parse("/memory launch plans"));
        assert_eq!(cmd.args, "launch plans");
        assert!(parsed(TEST_REGISTRY.parse("/memory")).args.is_empty());
    }

    #[test]
    fn required_args_produce_usage_when_missing() {
        match TEST_REGISTRY.parse("/background") {
            ParseResult::Usage(def, usage) => {
                assert_eq!(def.name, "background");
                assert_eq!(usage, "usage: /background <prompt>");
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn choice_args_validate_against_the_set() {
        assert_eq!(parsed(TEST_REGISTRY.parse("/voice ON")).args, "on");
        assert!(parsed(TEST_REGISTRY.parse("/voice")).args.is_empty());
        match TEST_REGISTRY.parse("/voice loud") {
            ParseResult::Usage(_, usage) => {
                assert_eq!(usage, "usage: /voice [on|off|status]");
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn smart_dashes_are_normalized_in_args() {
        // iOS turns "--preview" into "—preview" and "-n" into "–n".
        assert_eq!(
            parsed(TEST_REGISTRY.parse("/memory \u{2014}preview \u{2013}n")).args,
            "--preview -n"
        );
    }

    #[test]
    fn help_lists_every_command_exactly_once() {
        let help = REGISTRY.help_text();
        for def in COMMANDS {
            let count = help
                .lines()
                .filter_map(|line| line.strip_prefix("- /"))
                .filter_map(|line| line.split([' ', ':']).next())
                .filter(|name| *name == def.name)
                .count();
            assert_eq!(count, 1, "help must list /{} exactly once", def.name);
        }
    }

    #[test]
    fn help_annotates_aliases() {
        let help = REGISTRY.help_text();
        assert!(help.contains("- /quiet (or /observe):"));
        assert!(help.contains("- /help (or /commands):"));
    }

    #[test]
    fn prompt_template_keys_are_registered() {
        // A PromptTemplate key that isn't in the prompt engine falls back to
        // raw text at runtime; catch the drift here instead.
        let engine = crate::prompts::engine::PromptEngine::new("en").expect("prompt engine");
        for def in COMMANDS {
            if let CommandHandler::Agent(AgentAction::PromptTemplate(template)) = def.handler {
                engine.render_static(template).unwrap_or_else(|_| {
                    panic!("/{} references unregistered template {template}", def.name)
                });
            }
        }
    }

    #[test]
    fn names_and_aliases_are_globally_unique() {
        // Resolution is case-insensitive, so uniqueness must be checked on
        // normalized names or a future "STATUS" entry would collide with
        // "status" at runtime while passing here.
        let mut seen = std::collections::HashSet::new();
        for def in COMMANDS {
            assert!(
                seen.insert(def.name.to_ascii_lowercase()),
                "duplicate command name {}",
                def.name
            );
            for alias in def.aliases {
                assert!(
                    seen.insert(alias.to_ascii_lowercase()),
                    "duplicate alias {alias}"
                );
            }
        }
    }
}
