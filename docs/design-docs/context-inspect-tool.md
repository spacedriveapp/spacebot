# Context Inspect Tool

A debug tool for cortex chat that spawns a branch to analyze the complete internal context of an associated channel. This allows cortex chat to see exactly what a channel's LLM sees on its next turn, enabling effective debugging of channel behavior.

## Problem

When debugging channel issues via cortex chat, the admin only sees a 50-message transcript summary injected into the cortex's system prompt. This is insufficient to understand:

- Why a channel is making certain decisions
- What the full system prompt contains (identity, bulletin, skills, capabilities)
- What tools are available with their exact schemas
- How much context is being used (compaction state)
- What the complete conversation history looks like with branch/worker results
- What the current status block shows

Admins need to see the **exact context** that the channel's LLM sees — not a summary, but the complete system prompt + history + tools + status that would be sent to the LLM on the next turn.

## Solution

Add a `context_inspect` tool to cortex chat that:

1. Spawns a branch (to avoid polluting cortex chat's history with massive channel context)
2. The branch gets a special `read_channel_context` tool
3. This tool builds the **full channel context** exactly as the channel would see it
4. The branch analyzes the context and returns conclusions to cortex chat

### Architecture

```
Cortex Chat (opened on channel page)
  ↓ calls context_inspect tool
  ↓ spawns Branch with special tool server
  ↓ Branch calls read_channel_context
  ↓ ChannelContextInspector builds full snapshot:
      • System prompt (identity + bulletin + skills + capabilities + status)
      • Tool definitions with complete schemas
      • Conversation history (full Vec<Message>)
      • Status block text
      • Context statistics (token counts, usage %)
  ↓ Branch analyzes and returns conclusion
  ↓ Cortex chat receives analysis
```

## Components

### 1. `ChannelContextInspector` Service

**File:** `src/agent/channel_context.rs` (new)

A reusable service that assembles the complete channel context for inspection purposes. Mirrors the logic in `Channel::build_system_prompt()` and `Channel::run_agent_turn()` but makes it accessible from outside the channel.

**Core type:**
```rust
pub struct ChannelContextSnapshot {
    pub channel_id: String,
    pub channel_name: Option<String>,
    pub system_prompt: String,
    pub tool_definitions: Vec<ToolDefinition>,
    pub history: Vec<Message>,
    pub status_text: String,
    pub stats: ContextStats,
}

pub struct ContextStats {
    pub system_prompt_tokens: usize,
    pub tool_defs_tokens: usize,
    pub history_tokens: usize,
    pub total_tokens: usize,
    pub context_window: usize,
    pub usage_percent: f32,
}
```

**Key method:**
```rust
impl ChannelContextInspector {
    pub async fn inspect_channel(&self, channel_id: &str) 
        -> Result<ChannelContextSnapshot>;
}
```

**Implementation steps:**

1. Look up channel state from the active channels registry
2. Build system prompt components:
   - Identity context (`RuntimeConfig::identity.load().render()`)
   - Memory bulletin (`RuntimeConfig::memory_bulletin.load()`)
   - Skills prompt (`RuntimeConfig::skills.load().render_channel_prompt()`)
   - Worker capabilities (`PromptEngine::render_worker_capabilities()`)
   - Conversation context (platform, server, channel name)
   - Status block (`ChannelState::status_block.read().await.render()`)
   - Available channels list
   - Org context (if configured)
   - Link context (if applicable)
3. Render full system prompt via `PromptEngine::render_channel_prompt_with_links()`
4. Clone conversation history (`ChannelState::history.read().await.clone()`)
5. Query tool server for tool definitions (`ToolServerHandle::get_tool_defs()`)
6. Calculate token estimates using `estimate_history_tokens()`
7. Return complete snapshot

**Dependencies:**
- Needs access to active channels (via shared registry in `AgentDeps`)
- Needs `RuntimeConfig` for dynamic context components
- Needs channel's `ChannelState` for history/status/tools

### 2. `ReadChannelContextTool`

**File:** `src/tools/read_channel_context.rs` (new)

A tool that branches can use to read the full internal context of a channel.

**Tool interface:**
```rust
pub struct ReadChannelContextArgs {
    pub channel_id: String,
}

pub struct ReadChannelContextOutput {
    pub channel_id: String,
    pub channel_name: Option<String>,
    pub system_prompt: String,
    pub tool_list: String,           // Formatted list of tools
    pub history_summary: String,     // Message count + preview
    pub status_block: String,
    pub stats: ContextStats,
    pub context_preview: String,     // First 1000 chars
    pub full_context_dump: String,   // Complete formatted output
}
```

**Tool description** (for LLM):
```markdown
Read the complete internal context of a channel exactly as it appears to the 
channel's LLM on its next turn. Returns:

- Full system prompt (identity, bulletin, skills, capabilities, status block)
- All available tools with complete parameter schemas
- Complete conversation history with all messages, branch results, worker results
- Token usage statistics and compaction state
- Context window utilization percentage

Use this to understand exactly what information and capabilities a channel has 
access to. Particularly useful for debugging unexpected channel behavior.

Parameters:
- channel_id: The channel to inspect (e.g., "telegram:123456" or "discord:guild:channel")
```

**Output formatting:**

The `full_context_dump` field provides a structured markdown report:

```markdown
# Channel Context: #{channel_name}

**Channel ID:** {channel_id}
**Context Usage:** {stats.total_tokens} / {stats.context_window} tokens ({stats.usage_percent}%)

## System Prompt ({stats.system_prompt_tokens} tokens)

{system_prompt}

## Available Tools ({tool_count} tools, {stats.tool_defs_tokens} tokens)

{tool_list}

## Conversation History ({message_count} messages, {stats.history_tokens} tokens)

{history_summary}

## Status Block

{status_block}

## Analysis

- Compaction risk: {if usage_percent > 80% then "HIGH" else "Normal"}
- Active processes: {branch_count} branches, {worker_count} workers
- Recent completions: {recent_completion_count}
```

### 3. `ContextInspectTool`

**File:** `src/tools/context_inspect.rs` (new)

The tool cortex chat invokes to spawn the inspection branch.

**Tool interface:**
```rust
pub struct ContextInspectArgs {
    /// Optional: specific aspect to focus analysis on
    pub focus: Option<String>,
}

pub struct ContextInspectOutput {
    pub branch_id: BranchId,
    pub channel_id: String,
    pub message: String,
}
```

**Tool description** (for LLM):
```markdown
Spawn a branch to deeply inspect the complete internal context of the channel 
you're currently viewing. 

The inspection branch will analyze:
- Full system prompt (identity, memory bulletin, skills, worker capabilities)
- Complete conversation history with all messages and delegated process results
- Status block showing active branches and workers
- All available tools with their complete parameter schemas
- Context usage statistics and compaction risk assessment

You can optionally specify a `focus` parameter to direct the analysis toward a 
specific aspect:
- "memory_influence" - How memories are affecting channel behavior
- "compaction_state" - Context usage and compaction risk
- "tool_availability" - What tools are available and properly configured
- "recent_history" - Analysis of recent conversation patterns
- Or any custom focus area

**Requirements:** Only works when cortex chat is opened on a channel page.

Returns a branch ID. The branch will analyze the context and send its 
conclusions back asynchronously.
```

**Implementation:**

```rust
impl Tool for ContextInspectTool {
    async fn call(&self, args: Self::Args) -> Result<Self::Output> {
        // Verify channel context is active
        let Some(channel_id) = &self.channel_context_id else {
            return Err(ContextInspectError(
                "No channel context active. Open cortex chat on a channel \
                 page to use context_inspect.".into()
            ));
        };
        
        // Build branch description and prompt
        let description = format!("Inspect context of channel {}", channel_id);
        let prompt = match args.focus {
            Some(focus) => format!(
                "Use the read_channel_context tool to inspect channel {}. \
                 Focus your analysis on: {}. Provide actionable insights and \
                 identify any issues or anomalies.",
                channel_id, focus
            ),
            None => format!(
                "Use the read_channel_context tool to inspect channel {}. \
                 Provide a comprehensive analysis of the channel's current state, \
                 including: context usage, active processes, recent conversation \
                 patterns, memory influence, and any visible issues or anomalies.",
                channel_id
            ),
        };
        
        // Spawn branch with inspection tool server
        let branch_id = self.spawn_inspection_branch(
            channel_id,
            &description,
            &prompt,
        ).await?;
        
        Ok(ContextInspectOutput {
            branch_id,
            channel_id: channel_id.clone(),
            message: format!(
                "Spawned inspection branch {}. The branch will analyze the \
                 complete internal context of {} and return detailed findings.",
                branch_id, channel_id
            ),
        })
    }
}

impl ContextInspectTool {
    async fn spawn_inspection_branch(
        &self,
        channel_id: &str,
        description: &str,
        prompt: &str,
    ) -> Result<BranchId> {
        // Build branch system prompt
        let rc = &self.deps.runtime_config;
        let prompt_engine = rc.prompts.load();
        let system_prompt = prompt_engine.render_branch_prompt(
            &rc.instance_dir.display().to_string(),
            &rc.workspace_dir.display().to_string(),
        )?;
        
        // Create inspection tool server with read_channel_context
        let tool_server = create_inspection_branch_tool_server(
            self.deps.memory_search.clone(),
            self.conversation_logger.clone(),
            self.channel_store.clone(),
            self.run_logger.clone(),
            self.channel_inspector.clone(),
            &self.deps.agent_id,
        );
        
        // Build branch with empty history (doesn't need channel history)
        let branch = Branch::new(
            Arc::from(channel_id),
            description,
            self.deps.clone(),
            system_prompt,
            vec![],  // Empty history - branch will read context via tool
            tool_server,
            **rc.branch_max_turns.load(),
        );
        
        let branch_id = branch.id;
        
        // Spawn branch task
        tokio::spawn(async move {
            if let Err(error) = branch.run(prompt).await {
                tracing::error!(
                    branch_id = %branch_id,
                    %error,
                    "context inspection branch failed"
                );
            }
        });
        
        Ok(branch_id)
    }
}
```

### 4. Tool Server Factory

**File:** `src/tools.rs` (modify existing)

Add a new factory function for inspection branches:

```rust
/// Create a tool server for context inspection branches.
///
/// Inspection branches are spawned by cortex chat's context_inspect tool to
/// analyze channel context. They get memory tools, channel_recall, worker_inspect,
/// and the special read_channel_context tool.
pub fn create_inspection_branch_tool_server(
    memory_search: Arc<MemorySearch>,
    conversation_logger: ConversationLogger,
    channel_store: ChannelStore,
    run_logger: ProcessRunLogger,
    channel_inspector: Arc<ChannelContextInspector>,
    agent_id: &str,
) -> ToolServerHandle {
    ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .tool(WorkerInspectTool::new(run_logger, agent_id.to_string()))
        .tool(ReadChannelContextTool::new(channel_inspector))
        .run()
}
```

### 5. Cortex Chat Integration

**File:** `src/agent/cortex_chat.rs` (modify existing)

Store `channel_context_id` in the session and pass to tool server:

```rust
pub struct CortexChatSession {
    pub deps: AgentDeps,
    pub tool_server: ToolServerHandle,
    pub store: CortexChatStore,
    pub channel_context_id: Option<String>,  // NEW
    send_lock: Mutex<()>,
}

impl CortexChatSession {
    pub fn new(
        deps: AgentDeps,
        tool_server: ToolServerHandle,
        store: CortexChatStore,
        channel_context_id: Option<String>,  // NEW
    ) -> Self {
        Self {
            deps,
            tool_server,
            store,
            channel_context_id,
            send_lock: Mutex::new(()),
        }
    }
}
```

**File:** `src/tools.rs` (modify `create_cortex_chat_tool_server`)

Add `context_inspect` tool to cortex chat:

```rust
#[allow(clippy::too_many_arguments)]
pub fn create_cortex_chat_tool_server(
    memory_search: Arc<MemorySearch>,
    conversation_logger: ConversationLogger,
    channel_store: ChannelStore,
    run_logger: ProcessRunLogger,
    channel_inspector: Arc<ChannelContextInspector>,  // NEW
    agent_id: &str,
    channel_context_id: Option<String>,  // NEW
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger.clone(), channel_store.clone()))
        .tool(WorkerInspectTool::new(run_logger.clone(), agent_id.to_string()))
        .tool(ContextInspectTool::new(  // NEW
            conversation_logger,
            channel_store,
            run_logger,
            channel_inspector,
            channel_context_id,
        ))
        .tool(ShellTool::new(workspace.clone(), sandbox.clone()))
        .tool(FileTool::new(workspace.clone()))
        .tool(ExecTool::new(workspace, sandbox));
    
    if browser_config.enabled {
        server = server.tool(BrowserTool::new(browser_config, screenshot_dir));
    }
    
    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
    }
    
    server.run()
}
```

## Data Dependencies

### Active Channels Registry

To enable context inspection, we need access to active channel state. Add to `AgentDeps`:

```rust
pub struct AgentDeps {
    // ... existing fields ...
    
    /// Registry of active channels for context inspection
    pub active_channels: Arc<RwLock<HashMap<ChannelId, ChannelState>>>,
}
```

**Population:** When `MessagingManager` creates a channel, register it:

```rust
// In MessagingManager::start_channel() or similar
let channel_state = ChannelState::new(/* ... */);
deps.active_channels.write().await.insert(channel_id.clone(), channel_state.clone());
```

**Cleanup:** When a channel is removed/closed, unregister it:

```rust
deps.active_channels.write().await.remove(&channel_id);
```

This allows `ChannelContextInspector` to look up any active channel by ID.

## Prompt Templates

### `prompts/en/tools/context_inspect.md.j2`

```markdown
Spawn a branch to deeply inspect the complete internal context of the channel you're currently viewing.

The inspection branch will analyze the channel's full LLM context, including:
- Complete system prompt (identity, memory bulletin, skills, worker capabilities, status block)
- Conversation history with all messages, branch conclusions, and worker results
- Available tools with complete parameter schemas
- Context usage statistics and compaction risk

Use this when debugging channel behavior or understanding why a channel is making certain decisions.

**Optional focus parameter:** Direct the analysis toward a specific aspect:
- "memory_influence" - How memories affect behavior
- "compaction_state" - Context usage and overflow risk
- "tool_availability" - Tool configuration status
- "recent_history" - Conversation pattern analysis
- Or specify any custom focus area

**Requirements:** Only works when cortex chat is opened on a channel page.
```

### `prompts/en/tools/read_channel_context.md.j2`

```markdown
Read the complete internal context of a channel exactly as its LLM sees it on the next turn.

Returns a comprehensive snapshot including:
- Full system prompt with all dynamic components
- Complete tool definitions with parameter schemas
- Entire conversation history
- Status block (active branches, workers, recent completions)
- Token usage statistics and compaction state

This is the exact context that would be sent to the channel's LLM. Use it to understand what information and capabilities the channel has access to.
```

## Implementation Phases

### Phase 1: Core Infrastructure
1. Create `src/agent/channel_context.rs` with `ChannelContextInspector`
2. Add `active_channels` registry to `AgentDeps`
3. Update `MessagingManager` to populate/clean registry
4. Add unit tests for context snapshot generation

### Phase 2: Tool Implementation
1. Create `src/tools/read_channel_context.rs`
2. Create `src/tools/context_inspect.rs`
3. Add tool prompt templates
4. Add `create_inspection_branch_tool_server()` factory

### Phase 3: Cortex Chat Integration
1. Modify `CortexChatSession` to store `channel_context_id`
2. Update `create_cortex_chat_tool_server()` to add `context_inspect` tool
3. Pass `channel_context_id` from API layer

### Phase 4: Testing
1. Integration test: Create channel, open cortex chat, call `context_inspect`
2. Verify branch receives full context via `read_channel_context`
3. Verify cortex chat receives analysis
4. Test error cases (no channel context, invalid channel ID)

## File Changes

**New files:**
- `src/agent/channel_context.rs` - `ChannelContextInspector` service
- `src/tools/context_inspect.rs` - `ContextInspectTool`
- `src/tools/read_channel_context.rs` - `ReadChannelContextTool`
- `prompts/en/tools/context_inspect.md.j2` - Tool description
- `prompts/en/tools/read_channel_context.md.j2` - Tool description

**Modified files:**
- `src/lib.rs` - Add `active_channels` to `AgentDeps`, re-export new types
- `src/tools.rs` - Add factories for inspection branch and update cortex chat factory
- `src/agent/cortex_chat.rs` - Store and use `channel_context_id`
- `src/api/cortex.rs` - Pass `channel_context_id` from request
- `src/messaging/manager.rs` - Populate/clean active channels registry

**Estimated complexity:** Medium
**Estimated lines:** ~800 lines

## Usage Example

Admin opens cortex chat on a misbehaving Discord channel:

```
Admin: why isn't this channel responding to me?

Cortex: Let me inspect the channel's internal context to diagnose the issue.
[calls context_inspect tool]

Branch (internal): [calls read_channel_context]
Branch (internal): I can see the channel context. The memory bulletin contains 
a decision-level memory blocking responses to this user due to a config issue. 
The memory has 0.9 importance and was created 2 hours ago. The channel's status 
block shows no active processes. Context usage is at 45%.

Cortex: I found the issue. There's a memory blocking responses to you:

Memory ID: 27d46718-f13a-41bb-bc4f-e6afa1452e95
Type: Decision
Importance: 0.9
Content: "James instructed Spacebot to stop responding to him after config 
advice went wrong."

Want me to delete that memory so the channel can respond again?
```

## Notes

- The inspection branch gets an **empty history** — it doesn't need channel context inheritance since it reads context via the tool
- The `read_channel_context` tool could be extended to support arbitrary channel IDs (not just associated channel) for cross-channel debugging
- Token estimation uses the same logic as compactor (`estimate_history_tokens()`)
- The `ChannelContextInspector` can be reused for other debug features (context dumps, health checks, etc.)
- Frontend doesn't need changes — this is purely a backend/tool enhancement
