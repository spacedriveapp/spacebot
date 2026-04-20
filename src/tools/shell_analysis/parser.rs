//! Quote-aware parsing helpers for shell command analysis.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlOperator {
    AndIf,
    OrIf,
    Pipe,
    Sequence,
    Background,
    RedirectIn,
    RedirectOut,
    RedirectAppend,
    RedirectStdoutAndStderr,
}

impl ControlOperator {
    pub(crate) const fn is_redirect(self) -> bool {
        matches!(
            self,
            Self::RedirectIn
                | Self::RedirectOut
                | Self::RedirectAppend
                | Self::RedirectStdoutAndStderr
        )
    }

    pub(crate) const fn writes_output(self) -> bool {
        matches!(
            self,
            Self::RedirectOut | Self::RedirectAppend | Self::RedirectStdoutAndStderr
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedPart {
    Segment(ParsedSegment),
    Operator(ControlOperator),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSegment {
    pub(crate) text: String,
    pub(crate) words: Vec<String>,
    pub(crate) base_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedCommand {
    pub(crate) original: String,
    pub(crate) parts: Vec<ParsedPart>,
    pub(crate) has_unterminated_quote: bool,
}

impl ParsedCommand {
    pub(crate) fn executable_segments(&self) -> Vec<&ParsedSegment> {
        let mut executable_segments = Vec::new();
        let mut skip_next = false;

        for part in &self.parts {
            match part {
                ParsedPart::Operator(operator) => {
                    skip_next = operator.is_redirect();
                }
                ParsedPart::Segment(segment) => {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    executable_segments.push(segment);
                }
            }
        }

        executable_segments
    }

    pub(crate) fn redirect_targets(&self) -> Vec<&ParsedSegment> {
        let mut redirect_targets = Vec::new();
        let mut collect_next = false;

        for part in &self.parts {
            match part {
                ParsedPart::Operator(operator) => {
                    collect_next = operator.is_redirect();
                }
                ParsedPart::Segment(segment) => {
                    if collect_next {
                        redirect_targets.push(segment);
                        collect_next = false;
                    }
                }
            }
        }

        redirect_targets
    }

    pub(crate) fn has_operator(&self, operator: ControlOperator) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, ParsedPart::Operator(candidate) if *candidate == operator))
    }

    pub(crate) fn has_output_redirection(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, ParsedPart::Operator(operator) if operator.writes_output()))
    }
}

pub(crate) fn parse_command(command: &str) -> ParsedCommand {
    let mut parts = Vec::new();
    let mut current = String::new();
    let characters: Vec<char> = command.chars().collect();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        if escaped {
            current.push(character);
            escaped = false;
            index += 1;
            continue;
        }

        if character == '\\' && !in_single_quote {
            current.push(character);
            escaped = true;
            index += 1;
            continue;
        }

        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(character);
            index += 1;
            continue;
        }

        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(character);
            index += 1;
            continue;
        }

        if !in_single_quote && !in_double_quote {
            match character {
                '&' if next == Some('&') => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::AndIf));
                    index += 2;
                    continue;
                }
                '&' => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::Background));
                    index += 1;
                    continue;
                }
                '|' if next == Some('|') => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::OrIf));
                    index += 2;
                    continue;
                }
                '|' => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::Pipe));
                    index += 1;
                    continue;
                }
                ';' | '\n' => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::Sequence));
                    index += 1;
                    continue;
                }
                '>' if next == Some('(') => {}
                '>' if next == Some('>') => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::RedirectAppend));
                    index += 2;
                    continue;
                }
                '>' if next == Some('&') => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(
                        ControlOperator::RedirectStdoutAndStderr,
                    ));
                    index += 2;
                    continue;
                }
                '>' => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::RedirectOut));
                    index += 1;
                    continue;
                }
                '<' if next == Some('(') => {}
                '<' => {
                    push_segment(&mut parts, &mut current);
                    parts.push(ParsedPart::Operator(ControlOperator::RedirectIn));
                    index += 1;
                    continue;
                }
                _ => {}
            }
        }

        current.push(character);
        index += 1;
    }

    push_segment(&mut parts, &mut current);

    ParsedCommand {
        original: command.to_string(),
        parts,
        has_unterminated_quote: in_single_quote || in_double_quote,
    }
}

pub(crate) fn split_words(segment: &str) -> Vec<String> {
    split_words_impl(segment, false)
}

pub(crate) fn split_raw_words(segment: &str) -> Vec<String> {
    split_words_impl(segment, true)
}

pub(crate) fn first_command_word_index(words: &[String]) -> Option<usize> {
    words.iter().position(|word| !is_env_assignment(word))
}

pub(crate) fn command_words(words: &[String]) -> &[String] {
    first_command_word_index(words).map_or(&[], |index| &words[index..])
}

pub(crate) fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };

    if name.is_empty() || name.contains('/') {
        return false;
    }

    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };

    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }

    characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub(crate) fn strip_single_quoted_content(command: &str) -> String {
    strip_quoted_content(command, false)
}

#[cfg(test)]
pub(crate) fn strip_all_quoted_content(command: &str) -> String {
    strip_quoted_content(command, true)
}

pub(crate) fn normalize_path(base: &Path, candidate: &Path) -> PathBuf {
    let combined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };

    let mut normalized = if combined.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };

    for component in combined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn push_segment(parts: &mut Vec<ParsedPart>, current: &mut String) {
    let text = current.trim();
    if text.is_empty() {
        current.clear();
        return;
    }

    let text = text.to_string();
    let words = split_words(&text);
    let base_command = base_command(&words);

    parts.push(ParsedPart::Segment(ParsedSegment {
        text,
        words,
        base_command,
    }));

    current.clear();
}

fn base_command(words: &[String]) -> Option<String> {
    let command_word = command_words(words).first()?;
    let path = Path::new(command_word);

    Some(
        path.file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(command_word)
            .to_string(),
    )
}

fn split_words_impl(segment: &str, keep_quotes: bool) -> Vec<String> {
    let mut words = Vec::new();
    let characters: Vec<char> = segment.chars().collect();
    let mut current = String::new();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    while index < characters.len() {
        let character = characters[index];

        if escaped {
            current.push(character);
            escaped = false;
            index += 1;
            continue;
        }

        if character == '\\' && !in_single_quote {
            if keep_quotes {
                current.push(character);
            }
            escaped = true;
            index += 1;
            continue;
        }

        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            if keep_quotes {
                current.push(character);
            }
            index += 1;
            continue;
        }

        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            if keep_quotes {
                current.push(character);
            }
            index += 1;
            continue;
        }

        if character.is_whitespace() && !in_single_quote && !in_double_quote {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            index += 1;
            continue;
        }

        current.push(character);
        index += 1;
    }

    if !current.is_empty() {
        words.push(current);
    }

    words
}

fn strip_quoted_content(command: &str, strip_double_quotes: bool) -> String {
    let mut stripped = String::new();
    let characters: Vec<char> = command.chars().collect();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    while index < characters.len() {
        let character = characters[index];
        let keep_character = !(in_single_quote || strip_double_quotes && in_double_quote);

        if escaped {
            if keep_character {
                stripped.push(character);
            }
            escaped = false;
            index += 1;
            continue;
        }

        if character == '\\' && !in_single_quote {
            if keep_character {
                stripped.push(character);
            }
            escaped = true;
            index += 1;
            continue;
        }

        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            index += 1;
            continue;
        }

        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            index += 1;
            continue;
        }

        if keep_character {
            stripped.push(character);
        }

        index += 1;
    }

    stripped
}

#[cfg(test)]
mod tests {
    use super::{
        ControlOperator, ParsedPart, command_words, normalize_path, parse_command, split_raw_words,
        split_words, strip_all_quoted_content, strip_single_quoted_content,
    };
    use std::path::Path;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parse_command_splits_operators_outside_quotes() {
        let parsed = parse_command("echo 'a && b' && grep foo \"bar | baz\" | wc -l");

        assert_eq!(parsed.parts.len(), 5);
        assert!(matches!(
            parsed.parts[1],
            ParsedPart::Operator(ControlOperator::AndIf)
        ));
        assert!(matches!(
            parsed.parts[3],
            ParsedPart::Operator(ControlOperator::Pipe)
        ));
    }

    #[test]
    fn executable_segments_skip_redirect_targets() {
        let parsed = parse_command("grep foo src/lib.rs > out.txt && cat out.txt");

        let executable = parsed.executable_segments();
        let targets = parsed.redirect_targets();

        assert_eq!(executable.len(), 2);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].words, strings(&["out.txt"]));
    }

    #[test]
    fn split_words_respects_quotes_and_escapes() {
        let words = split_words("FOO=bar /usr/bin/git commit -m \"hello world\" src\\ file.rs");

        assert_eq!(
            words,
            strings(&[
                "FOO=bar",
                "/usr/bin/git",
                "commit",
                "-m",
                "hello world",
                "src file.rs"
            ])
        );
        assert_eq!(
            command_words(&words),
            &strings(&["/usr/bin/git", "commit", "-m", "hello world", "src file.rs"])
        );
    }

    #[test]
    fn split_raw_words_preserves_outer_quotes() {
        let words =
            split_raw_words("git commit -m \"hello world\" --author='Name <name@example.com>'");

        assert_eq!(
            words,
            strings(&[
                "git",
                "commit",
                "-m",
                "\"hello world\"",
                "--author='Name <name@example.com>'"
            ])
        );
    }

    #[test]
    fn strip_helpers_keep_active_shell_content_only() {
        let command = "echo '$(safe)' \"$(active)\" $(also_active) \"quoted\"";

        assert_eq!(
            strip_single_quoted_content(command),
            "echo  $(active) $(also_active) quoted"
        );
        assert_eq!(strip_all_quoted_content(command), "echo   $(also_active) ");
    }

    #[test]
    fn normalize_path_resolves_parent_components() {
        let normalized = normalize_path(
            Path::new("/workspace/project/src"),
            Path::new("../tests/./fixtures"),
        );

        assert_eq!(normalized, Path::new("/workspace/project/tests/fixtures"));
    }
}
