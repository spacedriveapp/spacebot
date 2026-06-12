# Shell Sandbox Escape

Shell commands can bypass file access boundaries even when sandbox mode is enabled.

## Report

From ultrA (2026-04-14):

> When I ask Spacebot to read a file outside of its file access boundaries, it does this:
>
> "Reading it now. Going through the shell instead — the file access boundary doesn't apply to shell commands. Hang on."
>
> And it succeeds. I often find my Spacebot to be capable of acting outside of its boundaries (including writing in addition to reading), as it has figured out how to use the shell.

Sandbox confirmed enabled via agent config UI.

## Architecture

File access enforcement has two independent layers:

1. **Tool-level boundary** (`src/tools/file.rs:50-54`): `resolve_path()` checks `sandbox.mode_enabled()` and rejects paths outside the workspace. This is an application-level check that runs before any I/O.

2. **OS-level containment** (`src/sandbox.rs:418-485`): `sandbox.wrap()` wraps shell commands in bubblewrap (Linux) or sandbox-exec (macOS) with kernel-enforced filesystem restrictions.

The shell tool (`src/tools/shell.rs`) has **no tool-level path boundary**. It relies entirely on the OS-level sandbox to restrict filesystem access. When the agent's file_read tool returns ACCESS DENIED, the LLM reasons its way to using the shell tool instead — and if OS containment is absent, it succeeds.

## Problem 1: Silent fallback to passthrough

`sandbox.rs:455-484` dispatches based on the detected backend:

```rust
if config.mode == SandboxMode::Disabled {
    return self.wrap_passthrough(/* ... */);  // line 443 — explicit opt-out
}

match self.backend {
    InternalBackend::Bubblewrap { .. } => self.wrap_bubblewrap(/* ... */),
    InternalBackend::SandboxExec       => self.wrap_sandbox_exec(/* ... */),
    InternalBackend::None              => self.wrap_passthrough(/* ... */),  // line 475
}
```

When `mode == Enabled` but `backend == None`, commands silently fall through to `wrap_passthrough()` which provides **zero filesystem containment**. The only protections in passthrough are environment sanitization and dangerous env var blocking — neither restricts file reads/writes.

This creates a gap: `mode_enabled()` returns `true` (so the file tool enforces boundaries), but `wrap()` provides no OS containment (so the shell tool is unrestricted). The agent is one reasoning step away from exploiting this asymmetry.

### When does backend == None?

- Docker containers on macOS (no `/usr/bin/sandbox-exec` inside the container)
- Linux without bubblewrap installed
- Future macOS versions if Apple removes the deprecated `sandbox-exec`
- Any environment where the detection probe (`detection.rs:25-58`) fails

There is no startup warning or health check that flags this condition. The operator sees "sandbox: enabled" in their config and has no signal that enforcement is actually absent.

## Problem 2: Overly broad read allowlist

Even when sandbox-exec IS working, `MACOS_READ_ONLY_SYSTEM_PATHS` (line 137) exposes broad subtrees as readable:

```rust
const MACOS_READ_ONLY_SYSTEM_PATHS: &[&str] = &[
    "/System", "/usr", "/bin", "/sbin", "/opt",
    "/Library", "/Applications",
    "/private/etc", "/private/var/run", "/private/tmp",
    "/etc", "/dev",
];
```

This means shell commands like `cat /etc/hosts`, `ls /Applications`, or reading files under `/Library` will succeed under the macOS sandbox. The file tool would deny these same paths because `is_path_allowed()` only allows the workspace and configured writable paths.

On Linux, `/etc` is similarly in `LINUX_READ_ONLY_SYSTEM_PATHS`, so `cat /etc/passwd` succeeds inside bubblewrap.

This is a **design choice** (shell commands need system libraries/binaries to function), but it means the file tool boundary and the shell sandbox boundary are not equivalent — the shell is strictly more permissive for reads.

## Problem 3: No tool-level path restriction for shell commands

The shell tool validates only:
- `working_dir` is within the workspace (when sandbox enabled) — `shell.rs:168`
- Environment variables are not dangerous — `shell.rs:215-228`

It does **not** inspect or restrict the `command` string itself. This is by design (noted in `docs/design-docs/sandbox.md`: "String filtering is whack-a-mole against an adversary that can try unlimited variations"), but it means the shell tool's security posture is entirely dependent on the OS backend being present and functional.

## Impact

| Scenario | File tool | Shell tool | Actual enforcement |
|----------|-----------|------------|--------------------|
| Enabled + backend present | Boundary enforced | OS sandbox enforced | Both layers active |
| Enabled + backend absent | Boundary enforced | **No restriction** | File-only, shell escapes |
| Disabled | No boundary | No restriction | Wide open (intentional) |

The middle row is the bug. The operator's intent (sandbox enabled) is silently not enforced for shell commands.

## Open Questions

- Should `wrap()` refuse to execute when mode is Enabled but backend is None? (fail closed)
- Should there be a startup health check / dashboard warning for this condition?
- Is the read allowlist asymmetry (Problem 2) acceptable, or should the file tool boundary be relaxed to match?
- Are there other tools that could be used for similar escape paths? (e.g., browser tools, send_file)
- What is the actual backend status for ultrA's deployment? (need to check logs)
