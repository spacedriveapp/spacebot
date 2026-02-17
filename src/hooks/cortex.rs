//! CortexHook: Prompt hook for system-level observer.

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};

/// Hook for cortex observation.
#[derive(Clone)]
pub struct CortexHook;

impl CortexHook {
    /// Create a new cortex hook.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CortexHook {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> PromptHook<M> for CortexHook
where
    M: CompletionModel,
{
    async fn on_completion_call(&self, _prompt: &Message, _history: &[Message]) -> HookAction {
        // Cortex observes but doesn't intervene
        tracing::trace!("cortex: completion call observed");
        HookAction::Continue
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        _response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        // Cortex observes system-level patterns
        // In the future, this could trigger memory consolidation, detect anomalies, etc.
        tracing::trace!("cortex: completion response observed");
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
    ) -> ToolCallHookAction {
        // Cortex tracks system-level tool usage patterns
        tracing::trace!(tool_name = %tool_name, "cortex: tool call observed");
        ToolCallHookAction::Continue
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        _result: &str,
    ) -> HookAction {
        // Cortex analyzes tool outcomes for system optimization
        tracing::trace!(tool_name = %tool_name, "cortex: tool result observed");
        HookAction::Continue
    }
}
