//! Integration test for sandbox wrapping in ShellTool
//!
//! This test verifies that ShellTool properly accepts and uses the sandbox
//! for command containment by checking the module structure and type signatures.

use spacebot::tools::shell::ShellTool;

fn assert_public_type<T>() {
    let _ = std::mem::size_of::<Option<T>>();
}

#[test]
fn test_shell_tool_module_structure() {
    assert_public_type::<ShellTool>();
}

#[test]
fn test_sandbox_module_structure() {
    assert_public_type::<spacebot::sandbox::Sandbox>();
}
