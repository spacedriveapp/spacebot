# Process Sandbox

OS-level filesystem containment for shell and exec tool subprocesses. Replaces the current string-based command filtering with kernel-enforced boundaries that no amount of LLM creativity can bypass.

## Context

Workers execute arbitrary shell commands via `sh -c`. The current security model uses string inspection to block dangerous commands before execution — checking for references to the instance directory, sensitive filenames, secret env vars, subshells, `eval`, interpreter one-liners, `/proc` paths, and env dump builtins. Around 180 lines of pattern matching in `shell.rs` and 30 in `exec.rs`.

This doesn't work. An LLM that hits a blocked command pivots to an equivalent that the string checks don't cover: `cp workspace/file /tmp/`, `cat > /tmp/file`, `find -exec`, heredocs, script files, language runtimes in non-`-c` mode, etc. The log from worker `022aa8f2` shows exactly this — the `file` tool correctly blocked a read outside workspace, so the worker used `shell` + `cp` instead and succeeded.

The `file` tool's path canonicalization + `starts_with` check is solid for its own operations. But shell/exec spawn real OS processes with full host access. String filtering is whack-a-mole against an adversary that can try unlimited variations.

## Current Security Inventory

Everything that exists today, what it does, and what happens to it.

### `shell.rs` — ShellTool

| Item | Lines | What it does | Disposition |
|------|-------|-------------|-------------|
| `SENSITIVE_FILES` constant | 12-18 | Blocklist of 5 filenames: `config.toml`, `config.redb`, `settings.redb`, `.env`, `spacebot.db` | **Remove.** Sandbox makes these read-only via mount namespace. |
| `SECRET_ENV_VARS` constant | 21-30 | Blocklist of 8 env var names: API keys for Anthropic, OpenAI, OpenRouter, Discord, Slack (bot + app), Telegram, Brave Search | **Remove.** Sandbox inherits the parent's env, but the sandbox blocks reading `/proc/self/environ`. Leak detection in `SpacebotHook` catches any secrets that make it into tool output. |
| `instance_dir` field | 36 | Stored on `ShellTool` for path blocking comparisons | **Remove.** No longer needed — sandbox handles containment. |
| `check_command()` method | 50-232 | Pre-execution string inspection with 9 categories of checks (detailed below) | **Remove entirely.** All 182 lines. |
| — Instance dir blocking | 53-69 | Blocks commands containing the instance dir path (unless they also mention workspace) | Replaced by bwrap `--ro-bind` making the instance dir read-only |
| — Sensitive file blocking | 71-90 | Blocks commands mentioning any of the 5 `SENSITIVE_FILES` names unless clearly targeting workspace | Replaced by bwrap mount namespace — files aren't writable |
| — Secret env var expansion | 92-118 | Blocks `$VAR`, `${VAR}`, `printenv VAR` patterns for 8 secret var names, including unbraced `$VAR` at word boundaries | Replaced by sandbox env isolation + leak detection hook |
| — Broad env dumps | 120-144 | Blocks bare `printenv`, `env`, `env |`, `env >` | Replaced by sandbox env isolation + leak detection hook |
| — Shell builtin dumps | 146-163 | Blocks `set`, `declare -p`, `export -p`, `compgen -e`, `compgen -v` as standalone commands or in pipes/chains | Replaced by sandbox env isolation + leak detection hook |
| — Subshell blocking | 165-178 | Blocks backticks, `$(...)`, `<(...)`, `>(...)` | **Remove.** These were only blocked to prevent dynamic construction of blocked commands. With real sandboxing, there's nothing to dynamically bypass. |
| — eval/exec blocking | 180-186 | Blocks `eval` and `exec` builtins | **Remove.** Same rationale — no string checks to bypass. |
| — Interpreter one-liners | 188-205 | Blocks `python3 -c`, `python -c`, `perl -e`, `ruby -e`, `node -e`, `node --eval` | **Remove.** These were blocked to prevent bypassing the string checks via a different language. Sandbox makes this irrelevant. |
| — /proc and /dev paths | 207-217 | Blocks `/proc/self/environ`, `/proc/*/environ`, `/dev/fd/`, `/dev/stdin` | Replaced by bwrap `--proc /proc` (fresh procfs) + leak detection hook |
| — Shell state dumps | 219-229 | Blocks `set`, `declare -p`, `compgen`, `export` builtins (partial duplicate of lines 146-163) | Replaced by sandbox env isolation + leak detection hook |
| `contains_shell_builtin()` helper | 423-434 | Splits command on `|;& ` and checks if a segment starts with a given builtin | **Remove.** Only used by `check_command()`. |
| Working dir validation | 316-333 | Canonicalizes `working_dir` arg and checks `starts_with(workspace)` | **Keep.** Gives the LLM a descriptive error instead of a confusing cwd-not-found from bwrap. |
| `tools/bin` PATH prepend | 352-357 | Prepends `{instance_dir}/tools/bin` to PATH | **Keep**, but needs adjustment — the sandbox module will handle passing this path through since `instance_dir` is being removed from ShellTool. |
| System-internal `shell()` fn | 438-471 | Bypasses all checks, used by the system (not LLM-facing) | **Keep unchanged.** Not LLM-facing, no sandbox wrapping needed. |
| `ShellError`, `ShellArgs`, `ShellOutput` types | 236-275 | Tool types | **Keep unchanged.** |
| `format_shell_output()` | 399-419 | Output formatting | **Keep unchanged.** |
| `ShellResult` type + `format()` | 474-487 | System-internal result type | **Keep unchanged.** |

### `exec.rs` — ExecTool

| Item | Lines | What it does | Disposition |
|------|-------|-------------|-------------|
| `instance_dir` field | 15 | Stored on `ExecTool` for path blocking comparisons | **Remove.** |
| `check_args()` method | 29-59 | Pre-execution arg inspection with 2 checks | **Remove entirely.** All 31 lines. |
| — Instance dir blocking | 37-43 | Blocks if joined args contain instance dir but not workspace | Replaced by sandbox mount namespace |
| — Sensitive file blocking | 45-56 | Blocks args mentioning `SENSITIVE_FILES` unless clearly in workspace | Replaced by sandbox mount namespace |
| Secret env var blocking | 203-213 | Blocks setting any of the 8 `SECRET_ENV_VARS` via the `env` parameter | **Remove.** Sandbox prevents writing to protected paths. The env vars themselves are process-inherited and the LLM can't change them via this tool anyway — this check was blocking the LLM from *naming* a secret var as a key, not from accessing the value. |
| `DANGEROUS_ENV_VARS` blocking | 215-244 | Blocks setting 12 dangerous env vars: `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`, `PYTHONPATH`, `PYTHONSTARTUP`, `NODE_OPTIONS`, `RUBYOPT`, `PERL5OPT`, `PERL5LIB`, `BASH_ENV`, `ENV` | **Keep.** These enable code injection by altering how the child process loads libraries/modules. bwrap doesn't prevent env var injection — it only controls filesystem visibility. An LLM setting `LD_PRELOAD=/workspace/evil.so` is still dangerous even in a sandbox. |
| Working dir validation | 184-201 | Canonicalizes `working_dir` arg and checks `starts_with(workspace)` | **Keep.** Same rationale as ShellTool. |
| `tools/bin` PATH prepend | 256-261 | Prepends `{instance_dir}/tools/bin` to PATH | **Keep**, same adjustment needed as ShellTool. |
| System-internal `exec()` fn | 330-362 | Bypasses all checks, used by the system (not LLM-facing) | **Keep unchanged.** |
| All type definitions | 62-117 | `ExecError`, `ExecArgs`, `EnvVar`, `ExecOutput` | **Keep unchanged.** |
| `format_exec_output()` | 306-326 | Output formatting | **Keep unchanged.** |
| `ExecResult` type | 365-371 | System-internal result type | **Keep unchanged.** |

### `file.rs` — FileTool

| Item | Lines | What it does | Disposition |
|------|-------|-------------|-------------|
| `resolve_path()` method | 26-75 | Canonicalizes path, checks `starts_with(workspace)`, rejects symlinks | **Keep unchanged.** This is in-process I/O, not subprocess spawning. The sandbox doesn't apply here. |
| `best_effort_canonicalize()` | 81-106 | Walks up to deepest existing ancestor for paths that don't fully exist yet | **Keep unchanged.** Used by `resolve_path()`. |
| Protected identity files | 203-216 | Blocks writes to `SOUL.md`, `IDENTITY.md`, `USER.md` (case-insensitive) | **Keep unchanged.** Application-level protection, not security boundary. |
| System-internal `file_read/write/list` | 362-404 | Bypass workspace containment, used by the system | **Keep unchanged.** |

### `send_file.rs` — SendFileTool

| Item | Lines | What it does | Disposition |
|------|-------|-------------|-------------|
| `call()` method | 81-144 | Accepts any absolute path, reads file, sends as attachment. Only checks: is_absolute, is_file, max 25MB | **Fix.** Add workspace path validation (same `resolve_path` pattern from FileTool). Currently any readable file on disk can be exfiltrated via this tool. |

### `hooks/spacebot.rs` — SpacebotHook (Leak Detection)

| Item | Lines | What it does | Disposition |
|------|-------|-------------|-------------|
| `match_patterns()` | 47-75 | 11 regex patterns matching secret prefixes: `sk-*` (OpenAI), `sk-ant-*` (Anthropic), `sk-or-*` (OpenRouter), PEM private keys, `ghp_*` (GitHub), `AIza*` (Google), Discord bot tokens, `xoxb-*` (Slack bot), `xapp-*` (Slack app), Telegram bot tokens, `BSA*` (Brave) | **Keep unchanged.** |
| `scan_for_leaks()` | 82-135 | Multi-layer content scanning: raw plaintext, URL-decoded (`sk%2Dant%2D...`), base64-decoded (standard + URL-safe), hex-decoded. Min 24 chars for base64, 40 hex chars to reduce false positives | **Keep unchanged.** |
| `on_tool_call()` hook | 175-218 | Scans tool arguments before execution. If leak found, returns `ToolCallHookAction::Skip` (tool call is not executed) | **Keep unchanged.** |
| `on_tool_result()` hook | 220-293 | Scans tool output after execution. If leak found, returns `HookAction::Terminate` (agent is killed to prevent exfiltration via subsequent tool calls) | **Keep unchanged.** |

### `browser.rs` — BrowserTool (SSRF Protection)

| Item | Lines | What it does | Disposition |
|------|-------|-------------|-------------|
| `validate_url()` | 33-80 | Blocks non-http/https schemes, cloud metadata endpoints (169.254.169.254, metadata.google.internal, metadata.aws.internal), private/loopback/link-local/CGNAT/broadcast/unspecified IPs | **Keep unchanged.** Orthogonal to filesystem sandboxing. |
| `is_blocked_ip()` | 84-102 | Checks IPv4 loopback/private/link-local/broadcast/unspecified/CGNAT and IPv6 loopback/unique-local/link-local/v4-mapped ranges | **Keep unchanged.** |

## Summary of Changes

**Removed (~215 lines):**
- `shell.rs`: `check_command()` (182 lines), `contains_shell_builtin()` (12 lines), `SENSITIVE_FILES` (7 lines), `SECRET_ENV_VARS` (10 lines), `instance_dir` field, `check_command()` call site in `call()`
- `exec.rs`: `check_args()` (31 lines), `instance_dir` field, `check_args()` call site in `call()`, secret env var blocking (10 lines)

**Kept:**
- `exec.rs`: `DANGEROUS_ENV_VARS` blocking (library injection protection)
- `shell.rs` + `exec.rs`: working dir validation, `tools/bin` PATH prepend, system-internal functions
- `file.rs`: everything (resolve_path, symlink check, identity file protection)
- `hooks/spacebot.rs`: everything (leak detection — plaintext, URL-encoded, base64, hex)
- `browser.rs`: everything (SSRF protection)

**Fixed:**
- `send_file.rs`: add workspace path validation (currently has none)

## Design

### Config

New `sandbox` section on the agent config:

```toml
[[agents]]
id = "main"

[agents.sandbox]
# "enabled" (default) - kernel-enforced filesystem containment
# "disabled" - full host access (self-hosted/local only)
mode = "enabled"

# Additional directories the agent can write to beyond its workspace.
# The workspace itself is always writable when sandbox is enabled.
writable_paths = ["/home/user/projects/myapp"]
```

### Types

```rust
pub struct SandboxConfig {
    pub mode: SandboxMode,
    pub writable_paths: Vec<PathBuf>,
}

pub enum SandboxMode {
    Enabled,   // OS-level containment (default)
    Disabled,  // No containment, full host access
}
```

Added to `AgentConfig` as `sandbox: Option<SandboxConfig>`, resolved to `ResolvedAgentConfig` with defaults (`mode = Enabled`, `writable_paths = []`). Added to `RuntimeConfig` as `ArcSwap<SandboxConfig>` for hot-reload.

### Sandbox Module

New module at `src/sandbox.rs`.

```rust
pub struct Sandbox {
    mode: SandboxMode,
    workspace: PathBuf,
    writable_paths: Vec<PathBuf>,
    backend: SandboxBackend,
}

enum SandboxBackend {
    Bubblewrap,      // Linux: bwrap available
    SandboxExec,     // macOS: /usr/bin/sandbox-exec
    None,            // No sandbox support detected, or mode = Disabled
}
```

#### Detection

At agent startup, probe for sandbox support:

- **Linux:** Check if `bwrap --version` succeeds, then run a preflight: `bwrap --ro-bind / / --proc /proc -- /usr/bin/true`. If `--proc /proc` fails (nested container restrictions), retry without it and remember the fallback.
- **macOS:** Check if `/usr/bin/sandbox-exec` exists. Hardcode the full path to prevent PATH injection.
- **Either platform, mode = Disabled:** `SandboxBackend::None`.
- **Backend unavailable but mode = Enabled:** Log a warning at startup. Fall back to `None` rather than refusing to start — degraded security is better than a broken agent.

#### Command Wrapping

`Sandbox` exposes a single method that transforms a `tokio::process::Command` before spawning:

```rust
impl Sandbox {
    /// Wrap a command for sandboxed execution.
    /// Returns the (possibly modified) Command ready to spawn.
    pub fn wrap(&self, program: &str, args: &[&str], working_dir: &Path) -> Command
}
```

**Linux (bubblewrap):**

```
bwrap
  --ro-bind / /                          # entire filesystem read-only
  --dev /dev                             # writable /dev with standard nodes
  --proc /proc                           # fresh /proc (skipped if preflight failed)
  --tmpfs /tmp                           # private tmp per invocation
  --bind <workspace> <workspace>         # workspace writable
  --bind <writable_path> <writable_path> # each configured writable path
  --ro-bind <data_dir> <data_dir>        # re-protect agent data dir
  --unshare-pid                          # isolated PID namespace
  --new-session                          # prevent TIOCSTI TTY injection
  --die-with-parent                      # kill child if spacebot dies
  --chdir <working_dir>                  # set cwd
  -- sh -c "<command>"                   # the actual command
```

Mount order matters — later mounts override earlier ones at the same path:
1. `--ro-bind / /` makes everything read-only
2. `--bind <workspace>` re-enables writes for the workspace
3. `--ro-bind <data_dir>` re-applies read-only on the data directory (which lives under the instance dir, potentially overlapping with workspace's parent)

The agent's `data_dir` (containing `spacebot.db`, `config.redb`, `settings.redb`, LanceDB) is explicitly re-mounted read-only even though `--ro-bind / /` already covers it. This ensures it stays protected if the workspace mount would otherwise make it writable (the default workspace is `{instance_dir}/agents/{id}/workspace`, and the data dir is `{instance_dir}/agents/{id}/data`).

**macOS (sandbox-exec):**

```
/usr/bin/sandbox-exec
  -p <generated SBPL profile>
  -DWORKSPACE=<workspace>
  -DWRITABLE_0=<writable_path>
  ...
  -- sh -c "<command>"
```

Generated SBPL profile:

```scheme
(version 1)
(deny default)

; process basics
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))

; filesystem: read everything, write workspace + configured paths
(allow file-read*)
(allow file-write* (subpath (param "WORKSPACE")))
; one (allow file-write* (subpath (param "WRITABLE_N"))) per configured path

; dev, sysctl, mach for basic operation
(allow file-write-data
  (require-all (path "/dev/null") (vnode-type CHARACTER-DEVICE)))
(allow sysctl-read)
(allow mach-lookup
  (global-name "com.apple.system.opendirectoryd.libinfo"))
(allow ipc-posix-sem)
(allow pseudo-tty)
```

All paths are canonicalized before being passed as params — `/var` on macOS is actually `/private/var`.

**Fallback (no backend):**

Pass through unchanged. The command runs unsandboxed. The warning at startup is the only indication.

### Tool Integration

`ShellTool` and `ExecTool` gain a `sandbox: Arc<Sandbox>` field. Set during tool server creation.

**ShellTool::call() changes:**

```rust
// Before (current):
let mut cmd = Command::new("sh");
cmd.arg("-c").arg(&args.command);
cmd.current_dir(&self.workspace);

// After:
let cmd = self.sandbox.wrap("sh", &["-c", &args.command], &working_dir);
```

**ExecTool::call() changes:**

```rust
// Before (current):
let mut cmd = Command::new(&args.program);
cmd.args(&args.args);
cmd.current_dir(&self.workspace);

// After:
let arg_refs: Vec<&str> = args.args.iter().map(|s| s.as_str()).collect();
let cmd = self.sandbox.wrap(&args.program, &arg_refs, &working_dir);
```

`ExecTool` retains the `DANGEROUS_ENV_VARS` check (blocking `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`, etc.) since these enable code injection regardless of filesystem sandbox state.

Both tools still set `stdout(Stdio::piped())`, `stderr(Stdio::piped())`, timeout, PATH, and output truncation after getting the wrapped command. The sandbox only affects how the process is spawned.

### Tool Server Wiring

`create_worker_tool_server()` and `create_cortex_chat_tool_server()` gain a `sandbox: Arc<Sandbox>` parameter and pass it to `ShellTool` and `ExecTool`:

```rust
pub fn create_worker_tool_server(
    // ... existing params ...
    sandbox: Arc<Sandbox>,
) -> ToolServerHandle {
    ToolServer::new()
        .tool(ShellTool::new(workspace.clone(), sandbox.clone()))
        .tool(FileTool::new(workspace.clone()))
        .tool(ExecTool::new(workspace, sandbox))
        // ... rest unchanged ...
}
```

The `Sandbox` is created once per agent during startup (in `initialize_agents()`) and shared via `Arc` across all workers for that agent.

### `send_file` Fix

The `send_file` tool (channel-only) currently reads any absolute path with no workspace check. Add the same `resolve_path` logic from `FileTool`:

```rust
async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
    let path = PathBuf::from(&args.file_path);
    let resolved = self.resolve_path(&path)?;  // same canonicalize + starts_with check
    // ... rest of existing logic using resolved path ...
}
```

### `tools/bin` Persistent Directory

Currently `{instance_dir}/tools/bin` is prepended to PATH so LLM-installed binaries persist. Under the sandbox, this directory is read-only (it's under instance_dir). Binaries there are still executable — the LLM can run them, but can't install new ones from inside a sandboxed shell.

Tool installation needs to happen through a dedicated non-sandboxed path (e.g., a `install_tool` tool or the skills system). This is a behavior change worth documenting but not a regression — the current system already has the same problem when `check_command()` blocks writes to the instance dir.

## Dockerfile Change

Add bubblewrap to both `slim` and `full` stages:

```dockerfile
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libsqlite3-0 curl bubblewrap \
    && ...
```

bubblewrap is ~50KB installed. Available in Debian bookworm's default repos.

## Dependencies

```toml
# None for bubblewrap — it's an external binary invoked via Command
# None for sandbox-exec — it's a macOS system binary

# Only if we later add Landlock as a secondary layer:
# landlock = "0.4"
```

No new Rust dependencies for the initial implementation. bubblewrap and sandbox-exec are invoked as subprocess wrappers via `tokio::process::Command`.

## File Changes

| File | Change |
|------|--------|
| `src/sandbox.rs` | New module: `Sandbox`, `SandboxConfig`, `SandboxMode`, `SandboxBackend`, detection, wrapping |
| `src/config.rs` | `SandboxConfig` on `AgentConfig`/`ResolvedAgentConfig`/`RuntimeConfig`, TOML parsing |
| `src/tools/shell.rs` | Remove `check_command()` (lines 50-232), `SENSITIVE_FILES` (lines 12-18), `SECRET_ENV_VARS` (lines 21-30), `contains_shell_builtin()` (lines 423-434), `instance_dir` field (line 36). Replace `instance_dir` in constructor with `sandbox: Arc<Sandbox>`. Replace `Command::new("sh")` block (lines 335-343) with `sandbox.wrap()`. Move `tools/bin` PATH setup into sandbox. Keep: working dir validation, timeout, output truncation, format, system-internal `shell()` fn |
| `src/tools/exec.rs` | Remove `check_args()` (lines 29-59), `instance_dir` field (line 15), secret env var blocking (lines 203-213). Replace `instance_dir` in constructor with `sandbox: Arc<Sandbox>`. Replace `Command::new(&args.program)` (line 246) with `sandbox.wrap()`. Move `tools/bin` PATH setup into sandbox. Keep: `DANGEROUS_ENV_VARS` blocking (lines 217-244), working dir validation, timeout, output truncation, format, system-internal `exec()` fn |
| `src/tools/send_file.rs` | Add `workspace: PathBuf` field, add `resolve_path()` method (reuse logic from `file.rs`), validate path in `call()` before reading |
| `src/tools.rs` | Update `create_worker_tool_server()` and `create_cortex_chat_tool_server()` signatures: drop `instance_dir` param, add `sandbox: Arc<Sandbox>` param. Pass `sandbox` to `ShellTool::new()` and `ExecTool::new()` |
| `src/lib.rs` | Add `pub mod sandbox` |
| `src/main.rs` | Create `Sandbox` during `initialize_agents()`, run preflight probe at startup |
| `Dockerfile` | Add `bubblewrap` to apt-get install in both `slim` and `full` stages |

## What This Doesn't Cover

- **Network isolation.** A sandboxed process can still `curl` data out. bubblewrap's `--unshare-net` can block all networking, but many legitimate tasks need network access (git clone, npm install, curl). Network policy is a separate design decision — could be a future `network_isolated` config flag, or a proxy-based approach like Codex uses.
- **Resource limits.** CPU and memory abuse. bubblewrap doesn't do cgroups. The existing timeout mechanism and Fly VM resource limits cover this for now.
- **Landlock as a secondary layer.** The `landlock` crate (v0.4) could add defense-in-depth inside the bwrap namespace. Not needed for v1 — bwrap's mount namespace isolation is sufficient. Worth revisiting if we want to restrict filesystem access within the sandbox more granularly (e.g., read-only access to specific subdirectories of the workspace).

## Phase Ordering

```
Phase 1 (sandbox module)    — Sandbox struct, detection, wrapping logic, SBPL generation
Phase 2 (tool integration)  — Wire into ShellTool, ExecTool, remove old checks
Phase 3 (config)            — SandboxConfig parsing, hot-reload, hosted enforcement
Phase 4 (send_file)         — Add workspace path validation
Phase 5 (Dockerfile)        — Add bubblewrap, verify on Fly
```

Phases 1-3 are tightly coupled and should land together. Phase 4 is independent. Phase 5 requires a Docker image rebuild and Fly deployment.
