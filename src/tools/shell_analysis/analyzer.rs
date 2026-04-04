//! High-level orchestration for shell command analysis.

use crate::tools::shell_analysis::categorizer::{CategorizationResult, categorize_command};
use crate::tools::shell_analysis::parser::{
    ParsedCommand, command_words, normalize_path, parse_command,
};
use crate::tools::shell_analysis::security::detect_patterns;
use crate::tools::shell_analysis::types::{
    CommandAnalysis, CommandCategory, DetectedPattern, DurationHint, PatternType, RiskLevel,
};

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct ShellAnalyzer {
    workspace: PathBuf,
}

impl ShellAnalyzer {
    pub(crate) fn new(workspace: PathBuf) -> Self {
        let workspace = normalize_path(Path::new("/"), &workspace);
        Self { workspace }
    }

    pub(crate) fn analyze(&self, command: &str, working_dir: &Path) -> CommandAnalysis {
        let normalized_working_dir = normalize_path(Path::new("/"), working_dir);
        let parsed = parse_command(command);
        let categorization = categorize_command(&parsed);
        let mut patterns = detect_patterns(command, &parsed);
        patterns.extend(self.detect_outside_workspace_paths(&parsed, &normalized_working_dir));

        let risk_level = assess_risk(&categorization, &patterns);
        let duration_hint = estimate_duration(&parsed, categorization.category);
        let confirmation_reason = confirmation_reason(&categorization, &patterns);
        let requires_confirmation = confirmation_reason.is_some();

        CommandAnalysis {
            category: categorization.category,
            risk_level,
            duration_hint,
            patterns,
            requires_confirmation,
            confirmation_reason,
            collapsed_by_default: categorization.collapsed_by_default,
            expects_no_output: categorization.expects_no_output,
        }
    }

    fn detect_outside_workspace_paths(
        &self,
        parsed: &ParsedCommand,
        working_dir: &Path,
    ) -> Vec<DetectedPattern> {
        for segment in parsed.executable_segments() {
            let words = command_words(&segment.words);
            for word in words.iter().skip(1) {
                if let Some(path) = resolve_candidate_path(working_dir, word)
                    && !path.starts_with(&self.workspace)
                {
                    return vec![DetectedPattern {
                        pattern_type: PatternType::OutsideWorkspacePath,
                        description: format!(
                            "Command references a path outside the workspace: {word}"
                        ),
                        position: None,
                    }];
                }
            }
        }

        for segment in parsed.redirect_targets() {
            for word in &segment.words {
                if let Some(path) = resolve_candidate_path(working_dir, word)
                    && !path.starts_with(&self.workspace)
                {
                    return vec![DetectedPattern {
                        pattern_type: PatternType::OutsideWorkspacePath,
                        description: format!(
                            "Command redirects to a path outside the workspace: {word}"
                        ),
                        position: None,
                    }];
                }
            }
        }

        Vec::new()
    }
}

fn assess_risk(categorization: &CategorizationResult, patterns: &[DetectedPattern]) -> RiskLevel {
    let mut risk_level = RiskLevel::Safe;

    if categorization.has_write
        || categorization.has_network
        || categorization.has_output_redirection
    {
        risk_level = RiskLevel::Caution;
    }

    if categorization.has_destructive {
        risk_level = RiskLevel::Dangerous;
    }

    for pattern in patterns {
        match pattern.pattern_type {
            PatternType::OutsideWorkspacePath => {
                if categorization.has_write
                    || categorization.has_output_redirection
                    || categorization.has_destructive
                {
                    return RiskLevel::Dangerous;
                }
                risk_level = promote_risk(risk_level, RiskLevel::Caution);
            }
            PatternType::CommandSubstitution
            | PatternType::ProcessSubstitution
            | PatternType::ObfuscatedFlag
            | PatternType::GitCommitMessage
            | PatternType::IfsInjection
            | PatternType::Newline
            | PatternType::CarriageReturn
            | PatternType::ProcEnvironAccess
            | PatternType::EnvExfiltration => {
                return RiskLevel::Dangerous;
            }
        }
    }

    risk_level
}

fn confirmation_reason(
    categorization: &CategorizationResult,
    patterns: &[DetectedPattern],
) -> Option<String> {
    let mut reasons = Vec::new();

    if categorization.has_destructive {
        reasons.push("Destructive commands require confirmation.".to_string());
    }

    for pattern in patterns {
        if pattern_requires_confirmation(pattern.pattern_type, categorization)
            && !reasons.iter().any(|reason| reason == &pattern.description)
        {
            reasons.push(pattern.description.clone());
        }
    }

    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join(" "))
    }
}

fn estimate_duration(parsed: &ParsedCommand, category: CommandCategory) -> DurationHint {
    let mut duration_hint = DurationHint::Fast;

    for segment in parsed.executable_segments() {
        let Some(base_command) = segment.base_command.as_deref() else {
            continue;
        };

        let words = command_words(&segment.words);
        let subcommand = words.get(1).map(String::as_str);

        match base_command {
            "apt" | "apt-get" | "brew" | "docker" | "make" | "nix" => {
                duration_hint = promote_duration(duration_hint, DurationHint::Long);
            }
            "bun" | "npm" | "pnpm" | "yarn" => {
                if matches!(
                    subcommand,
                    Some("add" | "build" | "install" | "test" | "update" | "upgrade")
                ) {
                    duration_hint = promote_duration(duration_hint, DurationHint::Long);
                } else {
                    duration_hint = promote_duration(duration_hint, DurationHint::Medium);
                }
            }
            "cargo" => {
                if matches!(
                    subcommand,
                    Some("build" | "check" | "clippy" | "doc" | "install" | "run" | "test")
                ) {
                    duration_hint = promote_duration(duration_hint, DurationHint::Long);
                }
            }
            "curl" | "wget" => {
                duration_hint = promote_duration(duration_hint, DurationHint::Medium);
            }
            "git" => {
                if matches!(
                    subcommand,
                    Some("clone" | "fetch" | "pull" | "push" | "submodule")
                ) {
                    duration_hint = promote_duration(duration_hint, DurationHint::Medium);
                }
            }
            _ => {}
        }
    }

    if category == CommandCategory::Network {
        promote_duration(duration_hint, DurationHint::Medium)
    } else {
        duration_hint
    }
}

fn pattern_requires_confirmation(
    pattern_type: PatternType,
    categorization: &CategorizationResult,
) -> bool {
    match pattern_type {
        PatternType::OutsideWorkspacePath => {
            categorization.has_write
                || categorization.has_output_redirection
                || categorization.has_destructive
        }
        PatternType::CommandSubstitution
        | PatternType::ProcessSubstitution
        | PatternType::ObfuscatedFlag
        | PatternType::GitCommitMessage
        | PatternType::IfsInjection
        | PatternType::Newline
        | PatternType::CarriageReturn
        | PatternType::ProcEnvironAccess
        | PatternType::EnvExfiltration => true,
    }
}

fn resolve_candidate_path(working_dir: &Path, word: &str) -> Option<PathBuf> {
    if word.is_empty() || word.starts_with('-') || word.starts_with('~') {
        return None;
    }

    if word.contains("://")
        || word.contains('$')
        || word.contains('*')
        || word.contains('?')
        || word.contains('[')
        || word.contains('{')
        || word.contains('`')
    {
        return None;
    }

    let looks_like_path = word.starts_with('/')
        || word.starts_with("./")
        || word.starts_with("../")
        || word == "."
        || word == ".."
        || word.contains('/');

    if !looks_like_path {
        return None;
    }

    Some(normalize_path(working_dir, Path::new(word)))
}

fn promote_duration(current: DurationHint, candidate: DurationHint) -> DurationHint {
    current.max(candidate)
}

fn promote_risk(current: RiskLevel, candidate: RiskLevel) -> RiskLevel {
    match (current, candidate) {
        (RiskLevel::Dangerous, _) | (_, RiskLevel::Dangerous) => RiskLevel::Dangerous,
        (RiskLevel::Caution, _) | (_, RiskLevel::Caution) => RiskLevel::Caution,
        _ => RiskLevel::Safe,
    }
}

#[cfg(test)]
mod tests {
    use super::ShellAnalyzer;
    use crate::tools::shell_analysis::types::{
        CommandCategory, DurationHint, PatternType, RiskLevel,
    };
    use std::path::Path;

    #[test]
    fn marks_read_only_searches_as_safe_and_collapsible() {
        let analyzer = ShellAnalyzer::new("/workspace/project".into());
        let analysis = analyzer.analyze(
            "cat Cargo.toml | grep serde",
            Path::new("/workspace/project"),
        );

        assert_eq!(analysis.category, CommandCategory::Other);
        assert_eq!(analysis.risk_level, RiskLevel::Safe);
        assert!(analysis.collapsed_by_default);
    }

    #[test]
    fn requires_confirmation_for_destructive_commands() {
        let analyzer = ShellAnalyzer::new("/workspace/project".into());
        let analysis = analyzer.analyze("rm -rf target", Path::new("/workspace/project"));

        assert_eq!(analysis.category, CommandCategory::Destructive);
        assert_eq!(analysis.risk_level, RiskLevel::Dangerous);
        assert!(analysis.requires_confirmation);
    }

    #[test]
    fn detects_outside_workspace_write_targets() {
        let analyzer = ShellAnalyzer::new("/workspace/project".into());
        let analysis = analyzer.analyze(
            "cp src/lib.rs ../backup/lib.rs",
            Path::new("/workspace/project"),
        );

        assert_eq!(analysis.risk_level, RiskLevel::Dangerous);
        assert!(
            analysis
                .patterns
                .iter()
                .any(|pattern| pattern.pattern_type == PatternType::OutsideWorkspacePath)
        );
        assert!(analysis.requires_confirmation);
    }

    #[test]
    fn marks_build_commands_as_long_running() {
        let analyzer = ShellAnalyzer::new("/workspace/project".into());
        let analysis = analyzer.analyze("cargo build --release", Path::new("/workspace/project"));

        assert_eq!(analysis.duration_hint, DurationHint::Long);
    }
}
