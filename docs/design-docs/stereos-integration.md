# stereOS Integration

Research into [stereOS](https://github.com/papercomputeco/stereos) — a NixOS-based Linux distro purpose-built for running AI agents in hardened VMs — and how it maps to Spacebot's architecture.

## What stereOS Is

stereOS produces bootable machine images called **mixtapes** that bundle a minimal, security-hardened Linux system with a specific AI agent binary. It boots in under 3 seconds, runs the agent in a sandboxed user account with a restricted PATH, and is controlled by two system daemons that handle lifecycle management, secret injection, and session introspection.

**Key properties:**

- NixOS-based, fully declarative — the entire OS is defined in Nix modules
- Targets lightweight VMs: QEMU/KVM, Apple Virtualization.framework
- Three image formats: raw EFI, QCOW2, direct-kernel boot artifacts
- Host-guest control plane over virtio-vsock (CID 3, port 1024) with TCP fallback
- Two orchestration daemons: `stereosd` (system lifecycle) and `agentd` (agent session management)
- Currently aarch64-linux only

**Existing mixtapes:** OpenCode, Claude Code, Gemini CLI, and a "full" variant bundling all three.

## Security Model

stereOS implements defense-in-depth across six layers:

1. **Restricted shell + PATH** — agent user gets a custom login shell that sets PATH to only a curated set of approved binaries. Nix tooling is never included. All Nix-related env vars are unset.
2. **Nix daemon ACL** — only root and wheel can talk to the Nix daemon. The agent user is in `agent` group, not `wheel`.
3. **Explicit sudo denial** — `agent ALL=(ALL:ALL) !ALL` before any permissive rules.
4. **Kernel hardening** — ptrace blocked (`yama.ptrace_scope=2`), kernel pointers hidden, dmesg restricted, core dumps disabled, ICMP redirects disabled.
5. **Immutable users, no passwords** — SSH keys injected ephemerally at boot by `stereosd` over vsock.
6. **VM boundary** — the VM itself is the primary isolation mechanism.

**Secret handling:** `stereosd` receives secrets from the host over vsock, writes them to `/run/stereos/secrets/` on tmpfs with root-only permissions (0700). Never touches disk.

## How It Maps to Spacebot

### 1. Hosted Platform Instance Runtime (High Value)

`spacebot-platform` currently provisions Fly Machines for customer instances. stereOS could serve as the instance runtime:

- Build a `spacebot-mixtape` containing the Spacebot binary
- `stereosd` handles lifecycle (start/stop/health) and secret injection (API keys, config) — replaces direct Fly Machine API calls for some operations
- Sub-3-second boot means near-instant instance provisioning
- `agentd` provides tmux session introspection — admins can attach to debug customer instances
- `mixtape.toml` manifest with SHA-256 checksums enables reproducible, verifiable deploys
- The NixOS declarative model means every instance runs an identical, auditable system

**Gap:** Fly uses Firecracker, not QEMU. Firecracker expects its own rootfs + kernel format, not raw EFI or QCOW2. stereOS already produces kernel artifacts (bzImage + initrd + cmdline) which is close to what Firecracker needs, but the rootfs extraction would require a new image format in stereOS. This is the main blocker for hosted platform use.

### 2. Worker Sandbox Environment (High Value, Longer Term)

Spacebot workers execute arbitrary shell commands via `ShellTool` and `ExecTool`. Since 0.2.0, sandboxing uses bubblewrap (Linux) and `sandbox-exec` (macOS) — see `docs/design-docs/sandbox.md`. stereOS offers a stronger primitive: **run worker processes inside a purpose-built VM**.

What this would look like:

- Spawn one stereOS VM per agent at agent boot time
- All workers for that agent execute inside the same VM
- Worker tools (shell, file, exec) execute inside the VM via a thin RPC layer over vsock
- The VM boundary replaces bubblewrap's mount namespace — kernel-level isolation instead of namespace-level
- stereOS's restricted PATH and sudo denial apply on top of the VM boundary
- Workers can't escape because they're in a different kernel

**Per-agent, not per-worker.** The VM boots once when the agent starts and stays up for the agent's lifetime. Workers spawn and die inside it. This is the right granularity because:

1. **Startup latency** — 2-3 seconds per VM is fine once at agent boot, but unacceptable if every fire-and-forget worker pays that cost. Workers spawn constantly; agents don't.
2. **Shared workspace** — all workers for an agent already share the same workspace and data directory. One VM matches the existing isolation boundary.
3. **Resource overhead** — one VM per agent (~128-256MB) is manageable. One per worker would balloon memory on instances with many concurrent workers.
4. **Matches bubblewrap** — the current sandbox design creates one `Sandbox` per agent at startup and shares it via `Arc` across all workers. Same logical boundary, stronger isolation.

**Tradeoffs vs bubblewrap:**

|                        | bubblewrap (current)                                              | stereOS VM (per-agent)                                                     |
| ---------------------- | ----------------------------------------------------------------- | -------------------------------------------------------------------------- |
| **Startup latency**    | ~0ms per worker                                                   | ~2-3 seconds once at agent boot, then ~0ms per worker                      |
| **Isolation strength** | Namespace-level (shared kernel)                                   | VM-level (separate kernel)                                                 |
| **Resource overhead**  | Minimal                                                           | ~128-256MB RAM per agent                                                   |
| **Network isolation**  | Not included (bwrap --unshare-net possible but breaks most tasks) | Controllable at VM level                                                   |
| **Complexity**         | Low (single bwrap call)                                           | High (VM lifecycle, vsock RPC, image distribution)                         |

**Verdict:** This is the right direction for high-security or multi-tenant scenarios (hosted platform), but overkill for self-hosted single-user instances. The bubblewrap sandbox should remain the default; VM isolation would be an opt-in upgrade for hosted deployments.

### 3. OpenCode Worker Backend (Medium Value)

Spacebot already supports OpenCode as a worker backend. stereOS ships an `opencode-mixtape`. There's a natural alignment — Spacebot could spawn OpenCode workers inside stereOS VMs for maximum isolation during coding sessions, especially for untrusted workloads. The plumbing already exists on both sides; it's mainly an integration question.

### 4. Self-Hosted Deployment (Medium Value)

For users who want to self-host Spacebot with maximum isolation, a `spacebot-mixtape` is the simplest path:

- `nix build .#spacebot-mixtape` produces a bootable image
- User launches it in QEMU/KVM or on Apple Silicon via `run-vm.sh`
- Secrets injected at boot, workspace mounted via virtio-fs
- No Docker, no container runtime, no host dependencies beyond QEMU

This is a clean alternative to the current Docker deployment path for security-conscious users.

## What a `spacebot-mixtape` Would Look Like

Adding a new mixtape to stereOS is minimal. Based on the existing patterns:

```nix
# mixtapes/spacebot/base.nix
{ config, lib, pkgs, ... }:
{
  stereos.agent.extraPackages = [ pkgs.spacebot ];

  # Seed a minimal config.toml — secrets come via stereosd at boot
  environment.etc."skel/.config/spacebot/config.toml".text = ''
    [llm]
    anthropic_key = "file:/run/stereos/secrets/ANTHROPIC_API_KEY"

    [[agents]]
    id = "main"
  '';
}
```

Then register in `flake.nix`:

```nix
spacebot-mixtape = stereos-lib.mkMixtape {
  name = "spacebot-mixtape";
  features = [ ./mixtapes/spacebot/base.nix ];
};
```

The main prerequisite is packaging the Spacebot binary as a Nix derivation. Since it's a single Rust binary with no runtime dependencies, this is straightforward — `crane` or `naersk` for the Nix build, with SQLite and OpenSSL as build inputs.

## Blockers and Open Questions

### Fly/Firecracker Compatibility

stereOS produces three image formats (raw EFI, QCOW2, kernel artifacts). Firecracker needs an ext4 rootfs and a kernel binary. stereOS's kernel artifacts output (bzImage + initrd) gets partway there, but Firecracker doesn't use initrd the same way — it expects a pre-mounted rootfs, not an initramfs that pivots. This would require either:

1. A new `formats/firecracker.nix` in stereOS that extracts the NixOS system closure into a flat ext4 image
2. Running QEMU/KVM on Fly instead of Firecracker (possible via Fly GPU machines or custom builders, but non-standard)

### Architecture Support

stereOS is aarch64-linux only. Fly Machines are predominantly x86_64. Cross-compilation in Nix is well-supported, so adding `x86_64-linux` is likely straightforward but untested.

### Control Plane Protocol

`stereosd` speaks a custom protocol over vsock. For `spacebot-platform` to manage stereOS instances, it would need a Rust client for this protocol (or stereOS would need an HTTP API). The protocol is not yet documented publicly — would need to inspect the `stereosd` source.

### Workspace Persistence

stereOS VMs are ephemeral by design. Spacebot instances need persistent storage (SQLite databases, LanceDB, workspace files). This would require virtio-fs mounts to a persistent volume, which stereOS supports but the Fly integration path would need to map to Fly volumes.

## Recommendation

**Near term (low effort, high signal):**

- Create a `spacebot-mixtape` in stereOS for self-hosted deployment. This is 2-3 files of Nix config + a Nix derivation for the Spacebot binary. Gives us a hardened, bootable deployment option with zero Docker dependency.

**Medium term (moderate effort):**

- Investigate Firecracker compatibility by looking at `stereosd`'s source and the Firecracker rootfs format. If the gap is small, stereOS becomes a candidate for the hosted platform runtime.

**Long term (high effort, high value):**

- Per-agent VM isolation for the hosted platform. One stereOS VM per agent, all workers execute inside it. This is the most architecturally interesting use case but requires solving vsock RPC, VM lifecycle management, and workspace mounting.

The bubblewrap sandbox (shipped in 0.2.0) remains the right default for most deployments. stereOS VM isolation is a complementary layer for high-security hosted scenarios, not a replacement.
