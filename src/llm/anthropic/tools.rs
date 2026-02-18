//! Claude Code tool name normalization.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Canonical Claude Code 2.x tool names.
const CLAUDE_CODE_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Lowercase → canonical casing lookup.
static CC_TOOL_LOOKUP: LazyLock<HashMap<String, &'static str>> = LazyLock::new(|| {
    CLAUDE_CODE_TOOLS
        .iter()
        .map(|&name| (name.to_lowercase(), name))
        .collect()
});

/// Normalize a tool name to Claude Code canonical casing (case-insensitive).
/// Unknown names pass through unchanged.
pub fn to_claude_code_name(name: &str) -> String {
    CC_TOOL_LOOKUP
        .get(&name.to_lowercase())
        .map(|&canonical| canonical.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Reverse-map a Claude Code canonical name back to the original tool name
/// from the request's tool definitions. Falls back to the input name if no
/// match is found.
pub fn from_claude_code_name(name: &str, original_tools: &[(String, String)]) -> String {
    let lower = name.to_lowercase();
    for (original_name, _description) in original_tools {
        if original_name.to_lowercase() == lower {
            return original_name.clone();
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_canonical_names_normalize() {
        for &name in CLAUDE_CODE_TOOLS {
            assert_eq!(to_claude_code_name(name), name);
        }
    }

    #[test]
    fn lowercase_normalizes_to_canonical() {
        assert_eq!(to_claude_code_name("read"), "Read");
        assert_eq!(to_claude_code_name("bash"), "Bash");
        assert_eq!(to_claude_code_name("askuserquestion"), "AskUserQuestion");
        assert_eq!(to_claude_code_name("webfetch"), "WebFetch");
    }

    #[test]
    fn mixed_case_normalizes() {
        assert_eq!(to_claude_code_name("READ"), "Read");
        assert_eq!(to_claude_code_name("bAsH"), "Bash");
        assert_eq!(to_claude_code_name("Grep"), "Grep");
    }

    #[test]
    fn unknown_names_pass_through() {
        assert_eq!(to_claude_code_name("custom_tool"), "custom_tool");
        assert_eq!(to_claude_code_name("MySpecialTool"), "MySpecialTool");
    }

    #[test]
    fn reverse_mapping_finds_original() {
        let tools = vec![
            ("my_read_tool".to_string(), "reads files".to_string()),
            ("my_bash".to_string(), "runs commands".to_string()),
        ];
        assert_eq!(from_claude_code_name("my_read_tool", &tools), "my_read_tool");
    }

    #[test]
    fn reverse_mapping_case_insensitive() {
        let tools = vec![
            ("read_file".to_string(), "reads files".to_string()),
        ];
        assert_eq!(from_claude_code_name("Read_File", &tools), "read_file");
    }

    #[test]
    fn reverse_mapping_falls_back_to_input() {
        let tools = vec![
            ("other_tool".to_string(), "does things".to_string()),
        ];
        assert_eq!(from_claude_code_name("Read", &tools), "Read");
    }
}
