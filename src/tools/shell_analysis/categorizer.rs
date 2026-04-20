//! Command categorization logic for shell analysis.

use crate::tools::shell_analysis::parser::{ParsedCommand, command_words};
use crate::tools::shell_analysis::types::CommandCategory;

use std::collections::HashSet;
use std::sync::LazyLock;

static SEARCH_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "ack", "ag", "find", "grep", "locate", "rg", "whereis", "which",
    ])
});

static READ_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "awk", "cat", "cut", "file", "head", "jq", "less", "more", "sort", "stat", "strings",
        "tail", "tr", "uniq", "wc",
    ])
});

static LIST_COMMANDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["du", "ls", "tree"]));

static WRITE_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "cp", "install", "ln", "mkdir", "mv", "tee", "touch", "truncate",
    ])
});

static DESTRUCTIVE_COMMANDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["dd", "mkfs", "rm", "shred"]));

static NETWORK_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "apt", "apt-get", "brew", "bun", "curl", "ftp", "npm", "pip", "pip3", "pnpm", "rsync",
        "scp", "sftp", "ssh", "telnet", "wget", "yarn",
    ])
});

static SILENT_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "cd", "chmod", "chgrp", "chown", "cp", "export", "ln", "mkdir", "mv", "rm", "rmdir",
        "touch", "unset", "wait",
    ])
});

static SEMANTIC_NEUTRAL_COMMANDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from([":", "echo", "false", "printf", "true"]));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CommandSemantics {
    is_search: bool,
    is_read: bool,
    is_list: bool,
    is_write: bool,
    is_destructive: bool,
    is_network: bool,
    is_silent: bool,
    is_neutral: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CategorizationResult {
    pub(crate) category: CommandCategory,
    pub(crate) collapsed_by_default: bool,
    pub(crate) expects_no_output: bool,
    pub(crate) has_write: bool,
    pub(crate) has_destructive: bool,
    pub(crate) has_network: bool,
    pub(crate) has_output_redirection: bool,
}

pub(crate) fn categorize_command(parsed: &ParsedCommand) -> CategorizationResult {
    let mut has_search = false;
    let mut has_read = false;
    let mut has_list = false;
    let mut has_write = false;
    let mut has_destructive = false;
    let mut has_network = false;
    let mut has_other = false;
    let mut has_non_neutral = false;
    let mut all_silent = true;

    for segment in parsed.executable_segments() {
        let semantics = segment_semantics(segment);
        if semantics.is_neutral {
            continue;
        }

        has_non_neutral = true;
        has_search |= semantics.is_search;
        has_read |= semantics.is_read;
        has_list |= semantics.is_list;
        has_write |= semantics.is_write;
        has_destructive |= semantics.is_destructive;
        has_network |= semantics.is_network;

        if !semantics.is_silent {
            all_silent = false;
        }

        if !(semantics.is_search
            || semantics.is_read
            || semantics.is_list
            || semantics.is_write
            || semantics.is_destructive
            || semantics.is_network
            || semantics.is_silent)
        {
            has_other = true;
        }
    }

    let has_output_redirection = parsed.has_output_redirection();
    if has_output_redirection {
        has_write = true;
        all_silent = false;
    }

    let collapsed_by_default = has_non_neutral
        && !has_write
        && !has_destructive
        && !has_network
        && !has_other
        && !has_output_redirection
        && (has_search || has_read || has_list);

    let expects_no_output = has_non_neutral
        && !has_output_redirection
        && !has_search
        && !has_read
        && !has_list
        && all_silent;

    let category = if has_destructive {
        CommandCategory::Destructive
    } else if has_network {
        CommandCategory::Network
    } else if has_write {
        CommandCategory::Write
    } else if !has_non_neutral {
        CommandCategory::Other
    } else {
        let family_count = usize::from(has_search) + usize::from(has_read) + usize::from(has_list);
        match family_count {
            0 if all_silent => CommandCategory::Silent,
            1 if has_search => CommandCategory::Search,
            1 if has_read => CommandCategory::Read,
            1 if has_list => CommandCategory::List,
            _ => CommandCategory::Other,
        }
    };

    CategorizationResult {
        category,
        collapsed_by_default,
        expects_no_output,
        has_write,
        has_destructive,
        has_network,
        has_output_redirection,
    }
}

fn segment_semantics(
    segment: &crate::tools::shell_analysis::parser::ParsedSegment,
) -> CommandSemantics {
    let mut semantics = CommandSemantics::default();
    let Some(base_command) = segment.base_command.as_deref() else {
        return semantics;
    };

    if SEMANTIC_NEUTRAL_COMMANDS.contains(base_command) {
        semantics.is_neutral = true;
        return semantics;
    }

    semantics.is_search = SEARCH_COMMANDS.contains(base_command);
    semantics.is_read = READ_COMMANDS.contains(base_command);
    semantics.is_list = LIST_COMMANDS.contains(base_command);
    semantics.is_write = WRITE_COMMANDS.contains(base_command);
    semantics.is_destructive = DESTRUCTIVE_COMMANDS.contains(base_command);
    semantics.is_network = NETWORK_COMMANDS.contains(base_command);
    semantics.is_silent = SILENT_COMMANDS.contains(base_command);

    let words = command_words(&segment.words);
    let subcommand = words.get(1).map(String::as_str);

    match base_command {
        "chmod" => {
            semantics.is_write = true;
            semantics.is_silent = true;
            if recursive_flag_present(words) {
                semantics.is_destructive = true;
            }
        }
        "chgrp" | "chown" => {
            semantics.is_write = true;
            semantics.is_silent = true;
        }
        "docker" => {
            if matches!(subcommand, Some("build" | "compose" | "pull" | "push")) {
                semantics.is_network = true;
            }
        }
        "git" => match subcommand {
            Some("checkout" | "switch") => {
                semantics.is_silent = true;
            }
            Some("clean") if force_flag_present(words) => {
                semantics.is_destructive = true;
                semantics.is_silent = true;
            }
            Some("clone" | "fetch" | "pull" | "push" | "submodule") => {
                semantics.is_network = true;
            }
            Some("reset") if long_flag_present(words, "--hard") => {
                semantics.is_destructive = true;
                semantics.is_silent = true;
            }
            _ => {}
        },
        "npm" | "bun" | "pnpm" | "yarn" => {
            if matches!(
                subcommand,
                Some("add" | "install" | "remove" | "update" | "upgrade")
            ) {
                semantics.is_network = true;
            }
        }
        "sed" => {
            if words
                .iter()
                .any(|word| word == "-i" || word.starts_with("-i") || word == "--in-place")
            {
                semantics.is_write = true;
            }
        }
        _ => {}
    }

    semantics
}

fn force_flag_present(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| word == "--force" || short_flag_present(word, 'f'))
}

fn long_flag_present(words: &[String], flag: &str) -> bool {
    words
        .iter()
        .any(|word| word == flag || word.starts_with(&format!("{flag}=")))
}

fn recursive_flag_present(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| word == "--recursive" || word.starts_with("-R"))
}

fn short_flag_present(word: &str, flag: char) -> bool {
    if !word.starts_with('-') || word.starts_with("--") {
        return false;
    }

    word.chars().skip(1).any(|candidate| candidate == flag)
}

#[cfg(test)]
mod tests {
    use super::categorize_command;
    use crate::tools::shell_analysis::parser::parse_command;
    use crate::tools::shell_analysis::types::CommandCategory;

    #[test]
    fn categorizes_simple_commands() {
        assert_eq!(
            categorize_command(&parse_command("ls -la")).category,
            CommandCategory::List
        );
        assert_eq!(
            categorize_command(&parse_command("grep foo src/lib.rs")).category,
            CommandCategory::Search
        );
        assert_eq!(
            categorize_command(&parse_command("rm -rf target")).category,
            CommandCategory::Destructive
        );
    }

    #[test]
    fn categorizes_compound_read_only_commands_as_collapsible() {
        let categorization = categorize_command(&parse_command("cat Cargo.toml | grep serde"));

        assert_eq!(categorization.category, CommandCategory::Other);
        assert!(categorization.collapsed_by_default);
    }

    #[test]
    fn categorizes_redirects_as_writes() {
        let categorization = categorize_command(&parse_command("ls > out.txt"));

        assert_eq!(categorization.category, CommandCategory::Write);
        assert!(categorization.has_output_redirection);
    }

    #[test]
    fn marks_silent_file_operations() {
        let categorization = categorize_command(&parse_command("mkdir tmp/output"));

        assert_eq!(categorization.category, CommandCategory::Write);
        assert!(categorization.expects_no_output);
    }

    #[test]
    fn detects_git_reset_as_destructive() {
        let categorization = categorize_command(&parse_command("/usr/bin/git reset --hard HEAD~1"));

        assert_eq!(categorization.category, CommandCategory::Destructive);
    }
}
