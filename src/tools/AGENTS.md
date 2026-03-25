# tools/

Tool implementations for all Spacebot process types. 40 files, 57 structs. Organized by function in the filesystem; by consumer in factory functions.

## ToolServer Topology

Each process type gets a purpose-built `ToolServer`. No process gets tools it shouldn't have.

| Process | Tools |
|---|---|
| **Channel** | `reply`, `branch`, `spawn_worker`, `route`, `cancel`, `skip`, `react` |
| **Branch** | `memory_save`, `memory_recall`, `memory_delete`, `channel_recall`, `spacebot_docs`, `task_create/list/update`, `spawn_worker` (optional) |
| **Worker** | `shell`, `file_*`, `set_status`, `task_update`, `browser_*` (conditional), `web_search`, `mcp_*` (dynamic), `read_skill`, `secret_set` |
| **Cortex** | `memory_save` |
| **Cortex Chat** | Branch + Worker superset + `config_inspect`, `spawn_worker` |
| **Factory** | `factory_create_agent`, `factory_list_presets`, `factory_load_preset`, `factory_search_context`, `factory_update_config`, `factory_update_identity` |

Channel tools are added and removed per turn because they hold per-turn state (e.g. the current message ID for `react`).

## Factory Functions

```
add_channel_tools()              — adds/removes per-turn channel tools
create_branch_tool_server()      — memory + recall + task board
create_worker_tool_server()      — shell + file + status + optional browser
create_cortex_tool_server()      — memory_save only
create_cortex_chat_tool_server() — full superset for interactive cortex
create_factory_tool_server()     — agent factory management tools
```

## Tool Trait Pattern

Every tool implements the Rig `Tool` trait:

```rust
impl Tool for MyTool {
    const NAME: &'static str = "my_tool";
    type Args = MyToolArgs;   // serde-deserializable input
    type Error = ToolError;
    type Output = MyToolOutput; // serde-serializable result

    fn definition(&self, _prompt: String) -> ToolDefinition { ... }
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> { ... }
}
```

`definition()` returns the JSON schema shown to the LLM. Doc comments on `Args` fields become the field descriptions the LLM reads, so write them as instructions, not descriptions.

Errors are returned as structured results, never panics. The LLM sees the error and can recover.

## Shared Context Patterns

**`SharedBrowserHandle`** — `Arc<Mutex<BrowserHandle>>` shared across all browser tools in a worker. One CDP session per worker, not one per tool call.

**`FileContext`** — carries the workspace root for path canonicalization. File tools reject any path that escapes the workspace root.

## Complex Tools

- **`browser.rs`** (2330 lines, 13 tools) — CDP accessibility API, SSRF protection, screenshot, navigation, form interaction
- **`file.rs`** (1013 lines, 4 tools) — read/write/edit/list with path canonicalization and workspace isolation
- **`spawn_worker.rs`** (558 lines, 2 variants) — fire-and-forget and interactive worker spawning

## Conventions

- **Output cap:** `MAX_TOOL_OUTPUT_BYTES = 50KB`, truncated at UTF-8 boundaries
- **Error-as-result:** tool errors go back to the LLM as structured output, not panics
- **Tool nudging:** workers must call `set_status(kind: "outcome")` before exiting; the hook enforces this with up to 2 retries before failing the worker
- **Doc comments = LLM instructions:** write them for the model, not for humans
