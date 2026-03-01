# Sandbox Hardening

Dynamic sandbox mode, hot-reload fix, capability manager, and policy enforcement for shell and OpenCode workers.

## 1. Dynamic Sandbox Mode (Hot-Reload Fix)

### Problem

Disabling the sandbox on an agent via the UI doesn't work. The setting visually reverts to "enabled" and the actual sandbox enforcement doesn't change. The config file on disk is written correctly, but the in-memory state is never updated.

### Root Cause

Three failures in the reload path:

1. **`reload_config()` skips sandbox.** `config.rs:5012-5014` has an explicit comment: "sandbox config is not hot-reloaded here because the Sandbox instance is constructed once at startup and shared via Arc. Changing sandbox settings requires an agent restart." Every other config field gets `.store(Arc::new(...))` in `reload_config()`, but `self.sandbox` is skipped.

2. **API returns stale data.** `get_agent_config()` reads from `rc.sandbox.load()` (`api/config.rs:232`), which still holds the startup value. The UI receives this stale response and resets the toggle.

3. **`Sandbox` struct stores mode as a plain field.** The `Sandbox` instance (`sandbox.rs:60-68`) captures `mode: SandboxMode` at construction. Even if the `RuntimeConfig.sandbox` ArcSwap were updated, the `Arc<Sandbox>` in `AgentDeps` would still enforce the old mode in `wrap()`.

### Sequence Diagram

```
UI: PUT /agents/config {sandbox: {mode: "disabled"}}
  -> api/config.rs writes mode="disabled" to config.toml      (correct)
  -> api/config.rs calls rc.reload_config()                    (skips sandbox)
  -> api/config.rs calls get_agent_config()                    (reads stale ArcSwap)
  -> returns {sandbox: {mode: "enabled"}}                      (wrong)
UI: displays "enabled"                                         (reverted)

~2s later:
  file watcher detects config.toml change
  -> calls reload_config() for all agents                      (skips sandbox again)
  -> all agents log "runtime config reloaded"                  (sandbox unchanged)
```

### Fix

#### Change 1: Update `RuntimeConfig.sandbox` in `reload_config()` (config.rs ~line 5011)

Add the sandbox store alongside the other fields. Remove the skip comment.

```rust
self.warmup.store(Arc::new(resolved.warmup));
self.sandbox.store(Arc::new(resolved.sandbox.clone()));

mcp_manager.reconcile(&old_mcp, &new_mcp).await;
```

This fixes the API response so `get_agent_config()` returns the correct value after a config change.

#### Change 2: Wrap `RuntimeConfig.sandbox` in `Arc` (config.rs:4920)

Change the field from `ArcSwap<SandboxConfig>` to `Arc<ArcSwap<SandboxConfig>>` so it can be shared with the `Sandbox` struct:

```rust
// Before
pub sandbox: ArcSwap<SandboxConfig>,

// After
pub sandbox: Arc<ArcSwap<SandboxConfig>>,
```

Update `RuntimeConfig::new()` accordingly:

```rust
// Before
sandbox: ArcSwap::from_pointee(agent_config.sandbox.clone()),

// After
sandbox: Arc::new(ArcSwap::from_pointee(agent_config.sandbox.clone())),
```

All existing `.load()` and `.store()` calls work through `Arc`'s `Deref` with no changes.

#### Change 3: Make `Sandbox` read mode dynamically (sandbox.rs)

Replace the `mode: SandboxMode` field with `config: Arc<ArcSwap<SandboxConfig>>`. Always detect the backend at startup (even when mode is initially Disabled), so we know what's available if the user later enables it.

```rust
pub struct Sandbox {
    config: Arc<ArcSwap<SandboxConfig>>,
    workspace: PathBuf,
    data_dir: PathBuf,
    tools_bin: PathBuf,
    backend: SandboxBackend,
}
```

Change `Sandbox::new()` signature to accept the shared ArcSwap:

```rust
pub async fn new(
    config: Arc<ArcSwap<SandboxConfig>>,
    workspace: PathBuf,
    instance_dir: &Path,
    data_dir: PathBuf,
) -> Self
```

Backend detection always runs. The initial mode only affects the startup log message.

In `wrap()`, read the current mode dynamically:

```rust
pub fn wrap(&self, program: &str, args: &[&str], working_dir: &Path) -> Command {
    let config = self.config.load();
    // ...
    if config.mode == SandboxMode::Disabled {
        return self.wrap_passthrough(program, args, working_dir, &path_env);
    }
    match self.backend {
        SandboxBackend::Bubblewrap { proc_supported } => { ... }
        SandboxBackend::SandboxExec => { ... }
        SandboxBackend::None => self.wrap_passthrough(program, args, working_dir, &path_env),
    }
}
```

The `writable_paths` field is removed from the struct. Paths are read from the ArcSwap config and canonicalized in `wrap()`. This is a cheap syscall and commands aren't spawned at rates where it matters.

#### Change 4: Pass the shared ArcSwap to `Sandbox::new()` at both construction sites

**main.rs ~line 1358:**

```rust
let sandbox = std::sync::Arc::new(
    spacebot::sandbox::Sandbox::new(
        runtime_config.sandbox.clone(),  // Arc<ArcSwap<SandboxConfig>>
        agent_config.workspace.clone(),
        &config.instance_dir,
        agent_config.data_dir.clone(),
    )
    .await,
);
```

**api/agents.rs ~line 665:** Same change.

### What Doesn't Change

- `AgentDeps.sandbox` stays as `Arc<Sandbox>` (lib.rs:214). The `Sandbox` itself now reads mode dynamically, so the `Arc<Sandbox>` reference doesn't need to be swapped.
- `ShellTool` and `ExecTool` continue holding `Arc<Sandbox>` and calling `.wrap()`. No changes needed.
- The bubblewrap and sandbox-exec wrapping logic is unchanged. Only the dispatch in `wrap()` reads the dynamic mode.

### Files Changed

| File | Change |
|------|--------|
| `src/config.rs` | `RuntimeConfig.sandbox` type to `Arc<ArcSwap<SandboxConfig>>`; `RuntimeConfig::new()` wraps in `Arc::new()`; `reload_config()` adds `self.sandbox.store()` and removes skip comment |
| `src/sandbox.rs` | `Sandbox.mode` field replaced with `config: Arc<ArcSwap<SandboxConfig>>`; `writable_paths` removed from struct, read dynamically; `Sandbox::new()` signature change; `wrap()` reads mode from ArcSwap; backend detection always runs |
| `src/main.rs` | Pass `runtime_config.sandbox.clone()` to `Sandbox::new()` |
| `src/api/agents.rs` | Same `Sandbox::new()` signature update |

---

## 2. Environment Sanitization

### Problem

Sandbox does NOT call `env_clear()`. Bubblewrap wrapping uses `--setenv PATH` but does not use `--clearenv`. Workers inherit the full parent environment. A worker can run `printenv ANTHROPIC_API_KEY` and get the raw key. Even `remove_var` on startup doesn't help because Linux `/proc/self/environ` is an immutable kernel snapshot from exec time.

MCP processes already do this correctly (`mcp.rs:309` calls `env_clear()`).

### Design

Sandbox `wrap()` must call `env_clear()` (or the bwrap equivalent `--clearenv`) and explicitly re-inject only approved variables. Three categories:

**Always passed through:**
- `PATH` (with tools/bin prepended, as today)
- `HOME`, `USER`, `LANG`, `TERM` (basic process operation)
- `TMPDIR` (if needed)

**Passed from secret store (tool secrets only):**
- Credentials for CLI tools workers invoke (`GH_TOKEN`, `GITHUB_TOKEN`, `NPM_TOKEN`, `AWS_*`, etc.)
- The secret store categorizes secrets as **system** (internal — LLM API keys, messaging tokens) or **tool** (external — CLI credentials). Only tool secrets are injected into worker subprocesses. System secrets stay in Rust memory and never enter any subprocess environment.
- `wrap()` reads the current tool secrets from the store and injects each via `--setenv` (bubblewrap) or `Command::env()` (passthrough/sandbox-exec).
- Skills that expect `GH_TOKEN` in the environment just work. Skills never see `ANTHROPIC_API_KEY` because it's a system secret.

**Passed from `passthrough_env` config (fallback for self-hosted without secret store):**
- A user-configured list of env var names to forward from the parent process: `passthrough_env = ["GH_TOKEN", "GITHUB_TOKEN"]`
- This is the escape hatch for self-hosted users who set env vars in Docker/systemd but don't configure a master key. Without it, `--clearenv` would silently strip their credentials.
- When the secret store is available, `passthrough_env` is redundant (everything should be in the store). The field still works — it's additive.
- See `docs/design-docs/secret-store.md` "Env Passthrough for Self-Hosted" for details.

**Always stripped:**
- All `SPACEBOT_*` internal vars (the master key is never in the environment — it lives in the OS credential store; see `docs/design-docs/secret-store.md`)
- All system secrets (LLM API keys, messaging tokens — see `docs/design-docs/secret-store.md`)
- Any env var not in the above three categories

For the passthrough (no sandbox) case: same env sanitization applies in the shell/exec tools directly via `Command::env_clear()` before `Command::env()` for the allowed + secret + passthrough vars.

This is a **hard prerequisite for the secret store** — see `docs/design-docs/secret-store.md`. The master key is protected independently by the OS credential store (Keychain / kernel keyring), but without `--clearenv`, system secrets and other sensitive env vars still leak to workers.

### Files Changed

| File | Change |
|------|--------|
| `src/sandbox.rs` | Add `--clearenv` to bubblewrap wrapping; add `env_clear()` to sandbox-exec and passthrough modes; re-add safe vars + tool secrets + `passthrough_env` vars |
| `src/config.rs` | Parse `passthrough_env: Vec<String>` in `SandboxConfig` |
| `src/tools/shell.rs` | Env sanitization for passthrough (no sandbox) mode |
| `src/tools/exec.rs` | Same env sanitization |

---

## 3. Durable Binary Location

### Problem

On hosted instances, binaries installed via `apt-get` land on the root filesystem which is ephemeral — machine image rollouts replace it. Any ad-hoc `apt-get install git` disappears on the next deploy. The agent reinstalls on demand, but this is slow and wastes turns.

### Existing Infrastructure

The durable path already works:

- `{instance_dir}/tools/bin` exists — hosted boot flow creates it (`mkdir -p "$SPACEBOT_DIR/tools/bin"`).
- `Sandbox` already prepends `tools/bin` to `PATH` for worker subprocesses (`sandbox.rs:138-149`).
- `/data` survives hosted machine image rollouts.

### Design

No internal registry, no install manager, no package-manager guards. The agent can install binaries however it wants — `apt-get`, `curl`, compile from source. The system's only job is to tell the agent where to put them so they survive.

#### Worker System Prompt Instruction

Workers get a line in their system prompt:

```
Persistent binary directory: /data/tools/bin (on PATH, survives restarts and rollouts)
Binaries installed via package managers (apt, brew, etc.) land on the root filesystem
which is ephemeral on hosted instances — they disappear on rollouts. To install a tool
durably, download or copy the binary into /data/tools/bin.
```

This is an instruction, not a guard. If the agent runs `apt install gh`, it works. The binary is ephemeral. If the agent is smart it downloads to `tools/bin` instead. If it's not, the binary disappears and it reinstalls next time. Not our problem to gatekeep.

#### Dashboard Observability (Optional)

A lightweight API endpoint that lists the contents of `tools/bin`:

**`GET /api/tools`**

```json
{
  "tools_bin": "/data/tools/bin",
  "binaries": [
    { "name": "git", "size": 3456789, "modified": "2026-02-15T10:30:00Z" },
    { "name": "gh", "size": 1234567, "modified": "2026-02-20T14:15:00Z" }
  ]
}
```

Just a directory listing — no state machine, no install locks, no checksums. The dashboard can render this as a "Tools" panel showing what's installed. Purely observational.

### Files Changed

| File | Change |
|------|--------|
| `prompts/worker.md` | Add durable binary location instruction |
| `src/api/tools.rs` | Optional: `GET /api/tools` directory listing endpoint |

---

## 4. OpenCode Auto-Allow (Independent Bug)

### Problem

OpenCode workers auto-allow all permission prompts (`opencode/worker.rs:429-432`). When OpenCode asks permission to run a bash command, the worker replies `PermissionReply::Once` unconditionally. OpenCode worker output is also not scanned by SpacebotHook, so leak detection doesn't apply.

This is an independent bug regardless of the other sections — OpenCode workers operate with no policy checks at all.

### Design

The auto-allow behavior is intentional for now (OpenCode needs to run commands to be useful as a worker backend). The fix is to wire OpenCode worker output through **both** protection layers that cover builtin workers:

1. **Output scrubbing (exact match)** — `StreamScrubber` from `src/secrets/scrub.rs` (see `secret-store.md`, Output Scrubbing). Replaces tool secret values with `[REDACTED:<name>]` in SSE events before they're forwarded. Uses the rolling buffer strategy to handle secrets split across SSE chunks. This runs first — proactive redaction.

2. **Leak detection (regex)** — shared regex patterns from `SpacebotHook` (see `src/hooks/spacebot.rs`). Scans for known API key formats (`sk-ant-*`, `ghp_*`, etc.) in the scrubbed output. If a match is found after scrubbing, it's a leak of a secret not in the store — kill the agent. This runs second — reactive safety net.

The sequencing matters: scrubbing first means stored tool secrets are redacted before leak detection runs, so leak detection only fires on unknown/unstored secrets. Without this order, leak detection would fire on every tool secret value (which is expected in worker output) and kill the agent unnecessarily.

Longer term, OpenCode's permission model could be integrated with the sandbox — permissions for bash commands routed through the same `wrap()` path. But that requires understanding OpenCode's permission protocol better (see Open Questions).

### Files Changed

| File | Change |
|------|--------|
| `src/opencode/worker.rs` | Wire SSE output through `StreamScrubber` (exact-match redaction) then leak detection (regex) before forwarding |
| `src/secrets/scrub.rs` | `StreamScrubber` — rolling buffer scrubber for chunked output (shared with other streaming paths) |
| `src/hooks/spacebot.rs` | Extract leak detection regex into a shared function callable from OpenCode worker |

---

## Tool Protection Audit

### File Tool Workspace Guard

Workers get `ShellTool`, `FileTool`, and `ExecTool` in the same toolbox. The file tool's `resolve_path()` workspace guard (`file.rs:26-75`) is security theater when sandbox is off:

- Worker wants to read `/data/config.toml`
- `FileTool.resolve_path()` rejects it — "outside workspace"
- Worker uses `ShellTool` with `cat /data/config.toml` — works fine, no sandbox enforcement
- Or `ExecTool` with `cat` — same thing

The file tool's check only prevents the LLM from using that specific tool for out-of-workspace access. It doesn't prevent anything because shell and exec are right there in the same toolbox with no equivalent restriction when sandbox is off.

When sandbox **is** on, the file tool's check is also redundant in the other direction — bwrap/sandbox-exec already makes everything outside the workspace read-only at the kernel level. The file tool runs in-process (not a subprocess), so the sandbox doesn't wrap it, but the workspace guard duplicates what the sandbox already enforces for writes. For reads, the sandbox allows reading everywhere (read-only mounts), and the file tool is actually **more restrictive** than the sandbox by blocking reads outside workspace too.

The file tool workspace guard has exactly one scenario where it provides unique value: **sandbox is on, and you want to prevent the LLM from reading files outside the workspace via the file tool** (since the sandbox allows reads everywhere). That's a defense-in-depth argument, not a security boundary. It's worth keeping for that reason, but it should not be confused with actual containment.

### Protection Matrix

Current state of protection across all tool paths, with sandbox disabled:

| Tool | Workspace Guard | Sandbox Enforcement | Env Inherited | Leak Detection | Net Protection |
|------|----------------|--------------------|----|-----|----|
| `file` (read/write/list) | Yes — `resolve_path()` blocks outside workspace | No (in-process, not a subprocess) | N/A | Yes (tool output scanned) | Workspace guard only — bypassable via shell/exec in the same toolbox |
| `shell` | Working dir validation only | Sandbox wraps subprocess — but disabled | Full parent env | Yes (args + output scanned) | Leak detection only |
| `exec` | Working dir validation only | Sandbox wraps subprocess — but disabled | Full parent env | Yes (args + output scanned) | Leak detection + dangerous env var blocklist |
| `send_file` | **None** — any absolute path | No (in-process read) | N/A | Yes (output scanned) | Leak detection only |
| `browser` | N/A | N/A | N/A | Yes (output scanned) | SSRF protection (blocks metadata endpoints, private IPs) |
| OpenCode workers | Workspace-scoped by OpenCode | Not sandboxed | Full parent env via OpenCode subprocess | No (OpenCode output not scanned by SpacebotHook) | Auto-allow on all permissions |

### Key Observations

- The file tool's workspace guard is the only tool-level path restriction, but it's trivially bypassed via shell/exec which are in the same toolbox. It gives a false sense of containment.
- With sandbox off, the only real protection across all tools is leak detection (reactive, pattern-based, kills the agent after the fact).
- `send_file` has no workspace validation at all — can exfiltrate any readable file as a message attachment. This is an independent bug regardless of sandbox state.
- OpenCode workers bypass both sandbox and leak detection. They inherit the full environment and auto-allow all permission prompts.
- The file tool guard's only real value is as read-containment when sandbox is on (preventing LLM from reading sensitive files outside workspace via the file tool specifically, since bwrap mounts everything read-only but still readable).

---

## Phase Plan

### Phase 1: Dynamic Sandbox Mode (Hot-Reload Fix)

Fix the user-facing bug where toggling sandbox mode via UI doesn't take effect. Changes to `config.rs`, `sandbox.rs`, `main.rs`, `api/agents.rs`.

1. Wrap `RuntimeConfig.sandbox` in `Arc`.
2. Add `self.sandbox.store()` to `reload_config()`.
3. Refactor `Sandbox` to read mode from `Arc<ArcSwap<SandboxConfig>>`.
4. Update both `Sandbox::new()` call sites.
5. Verify: change sandbox mode via API, confirm `GET /agents/config` returns the new value, confirm `wrap()` uses the new mode.

### Phase 2: Environment Sanitization

Prevent secret leakage through environment variable inheritance. Secret-store-aware — workers get tool secrets (CLI credentials) but never system secrets (LLM keys, messaging tokens) or internal vars.

1. Add `--clearenv` to bubblewrap wrapping, re-add only safe vars + tool secrets from the secret store.
2. Add `env_clear()` to sandbox-exec and passthrough wrapping modes with same allowlist.
3. Add `env_clear()` to shell/exec tools for the no-sandbox case.
4. Verify: worker running `printenv` shows PATH/HOME/LANG + tool secrets (e.g., `GH_TOKEN`), but NOT `ANTHROPIC_API_KEY` or other system/internal vars. (The master key is never in the environment — it lives in the OS credential store.)

### Phase 3: Durable Binary Instruction

Add the persistent binary location instruction to worker prompts and optional dashboard observability.

1. Add tools/bin instruction to worker system prompt.
2. Optionally add `GET /api/tools` directory listing endpoint.
3. Verify: worker prompt includes the durable path; dashboard shows installed tools.

### Phase 4: OpenCode Output Protection

Wire OpenCode worker output through both protection layers (output scrubbing + leak detection) that cover builtin workers.

1. Wire SSE output through `StreamScrubber` (exact-match redaction of tool secret values, rolling buffer for split secrets). Runs first — proactive.
2. Extract leak detection regex from SpacebotHook into a shared function.
3. Scan scrubbed SSE output through leak detection. Runs second — reactive safety net for secrets not in the store.
4. Verify: a stored tool secret in OpenCode output is redacted to `[REDACTED:<name>]`; an unknown secret pattern triggers the same kill behavior as builtin workers.

### Phase 5: send_file Workspace Validation

Fix the independent bug where `send_file` can exfiltrate any readable file.

1. Add workspace validation to `send_file` matching the file tool's `resolve_path()` pattern.
2. Verify: `send_file` with a path outside workspace returns an error.

---

## Open Questions

1. **OpenCode permission protocol.** Can OpenCode's permission model be integrated with the sandbox? Would need to route bash permission requests through `wrap()`. Requires understanding the OpenCode permission protocol internals.
2. **Dynamic writable_paths.** The hot-reload fix reads `writable_paths` from the ArcSwap on every `wrap()` call, canonicalizing each time. If an agent has many writable paths and spawns commands at high frequency, this could be optimized with a change-detection cache. Likely not a concern in practice.
3. **Tool secret injection interface.** How does `wrap()` get tool secrets? Options: (a) `Sandbox` holds an `Arc<SecretsStore>` and calls `tool_env_vars()` on each `wrap()`, (b) tool secrets cached in an `Arc<ArcSwap<HashMap<String, String>>>` updated when secrets change. Option (b) avoids decryption on every subprocess spawn. Tool secrets change rarely (only via dashboard), so the cache is almost always warm.
4. **Worker keyring isolation.** The `pre_exec` hook that gives workers a fresh session keyring (see `secret-store.md`) should be wired into `wrap()` alongside env sanitization. Both run regardless of sandbox state — `--clearenv` strips env vars, `keyctl(JOIN_SESSION_KEYRING)` strips keyring access. Need to verify this works correctly with bubblewrap's `--unshare-pid` (the keyring is per-session, not per-PID-namespace, so both should be independent).

---

## Future: True Sandboxing (VM Isolation via stereOS)

Everything above operates at the namespace level — bubblewrap restricts mounts, `--clearenv` strips the environment, policy guards block specific commands. These are necessary fixes but they share a fundamental limitation: the agent and the host share a kernel. A compromised or misconfigured bubblewrap invocation can be escaped. `/proc` attacks, kernel exploits, and symlink races all exist within the same kernel boundary.

stereOS (see `docs/design-docs/stereos-integration.md`) offers a stronger primitive: **run worker processes inside a purpose-built VM with a separate kernel**. This section captures how that maps to the sandbox architecture as a future upgrade path.

### Per-Agent VM, Not Per-Worker

The right granularity is one VM per agent, not one per worker:

- **Startup cost.** stereOS boots in ~2-3 seconds. Fine once at agent boot; unacceptable per fire-and-forget worker. Workers spawn constantly, agents don't.
- **Shared workspace.** All workers for an agent already share the same workspace and data directory. One VM matches the existing `Arc<Sandbox>` isolation boundary.
- **Resource overhead.** One VM per agent (~128-256MB RAM) is manageable. One per worker would balloon memory with concurrent workers.

The VM boots when the agent starts and stays up for the agent's lifetime. Workers spawn and die inside it. This directly parallels how the current `Sandbox` struct is constructed once at agent startup and shared via `Arc` across all workers.

### What Changes

Worker tool execution (shell, file, exec) currently calls `sandbox.wrap()` which prepends bubblewrap arguments to a `Command`. With VM isolation, these tools would instead dispatch commands over a vsock RPC layer to the agent's VM:

```
Current:  ShellTool → sandbox.wrap() → bwrap ... -- sh -c "command" (same kernel)
Future:   ShellTool → vm_rpc.exec()  → agentd → sh -c "command"   (guest kernel)
```

The `Sandbox` trait boundary stays the same — `wrap()` produces a `Command`. The VM backend would produce a `Command` that speaks vsock instead of forking a local subprocess. Tools don't need to know which backend they're using.

stereOS's `agentd` daemon already handles session management inside the VM. Worker commands would be dispatched as agentd sessions, with stdout/stderr streamed back over vsock.

### Security Model Upgrade

stereOS adds layers that bubblewrap cannot provide:

| Layer | bubblewrap (current) | stereOS VM |
|-------|---------------------|------------|
| **Kernel isolation** | Shared kernel (namespace-level) | Separate kernel (VM-level) |
| **PATH restriction** | Sandbox prepends `tools/bin` | Restricted shell with curated PATH, Nix tooling excluded |
| **Privilege escalation** | Relies on namespace user mapping | Explicit sudo denial (`agent ALL=(ALL:ALL) !ALL`), no wheel group |
| **Kernel hardening** | Host kernel settings apply | ptrace blocked, kernel pointers hidden, dmesg restricted, core dumps disabled |
| **Secret injection** | Env vars (cleared by `--clearenv`) | Written to tmpfs at `/run/stereos/secrets/` with root-only permissions (0700), never on disk |
| **User isolation** | UID mapping in namespace | Immutable users, no passwords, SSH keys injected ephemerally over vsock |

The secret injection model is particularly relevant. Today, the secret store design (see `docs/design-docs/secret-store.md`) protects the master key via the OS credential store (Keychain / kernel keyring) — workers can't access it regardless of sandbox state. With stereOS, an even stronger model is possible: the master key stays on the host entirely, never entering the VM. Secrets are injected individually into the guest's tmpfs by `stereosd`. The agent process inside the VM reads from `/run/stereos/secrets/`. The OS credential store and VM isolation are complementary — the credential store protects on the host, the VM boundary protects in the guest.

### Network Isolation

bubblewrap's `--unshare-net` exists but breaks most useful worker tasks (git clone, API calls, web browsing). It's all-or-nothing.

VM-level networking is controllable with more granularity. The host can configure the VM's virtual NIC to allow outbound connections to specific hosts/ports while blocking everything else. This enables scenarios like "workers can reach github.com and the OpenAI API but nothing else" — impossible with bubblewrap without a userspace proxy.

### Blockers

This is not ready to implement. Key gaps from the stereOS integration research:

1. **Fly/Firecracker format mismatch.** Fly Machines use Firecracker, which expects ext4 rootfs + kernel binary. stereOS produces raw EFI, QCOW2, and kernel artifacts (bzImage + initrd). Firecracker doesn't use initrd the same way. Either stereOS needs a `formats/firecracker.nix` output, or we run QEMU/KVM on Fly (non-standard).

2. **Architecture.** stereOS is aarch64-linux only. Fly Machines are predominantly x86_64. Cross-compilation in Nix is straightforward but untested for stereOS.

3. **Control plane protocol.** `stereosd` speaks a custom vsock protocol. `spacebot-platform` would need a Rust client, or stereOS would need an HTTP API layer. The protocol isn't documented publicly yet.

4. **Workspace persistence.** stereOS VMs are ephemeral by design. Spacebot needs persistent storage (SQLite, LanceDB, workspace files). Requires virtio-fs mounts to persistent volumes, which stereOS supports but the Fly integration path would need to map to Fly volumes.

### Relationship to Current Work

The phases above (1-5) are prerequisites, not alternatives:

- **Phases 1-2** (hot-reload fix, env sanitization) fix correctness bugs that matter regardless of backend. Even with VM isolation, the host process still needs `--clearenv` for any non-VM code paths (MCP processes, in-process tools).
- **Phase 3** (durable binary instruction) applies inside the VM too — the VM image would include `tools/bin` on the persistent volume mount.
- **Phases 4-5** (OpenCode leak detection, send_file fix) are bug fixes that apply regardless of sandbox backend.

bubblewrap remains the default sandbox backend for all deployments. VM isolation would be an opt-in upgrade for the hosted platform where multi-tenant security justifies the resource overhead. Self-hosted users who want maximum isolation could run a `spacebot-mixtape` directly (NixOS image, no Docker) as an alternative deployment path.

