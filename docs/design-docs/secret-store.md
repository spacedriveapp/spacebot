# Secret Store

Credential storage with two secret categories: system secrets (internal, never exposed) and tool secrets (passed to worker subprocesses as env vars). Works out of the box without encryption — the master key is an optional hardening layer that adds encryption at rest.

**Hard dependency:** Environment sanitization (sandbox-hardening.md, Phase 2) must ship before or alongside this. Without `--clearenv` in sandbox wrapping, system secrets and other env vars leak to workers.

## Current State

All secrets currently live in config.toml as plaintext:

**config.toml** — the vast majority of users (all hosted, most self-hosted) set up API keys through the dashboard's provider UI. The dashboard sends the key to `PUT /api/providers`, which writes the literal value directly into config.toml (`anthropic_key = "sk-ant-abc123..."`). The `env:` prefix (`anthropic_key = "env:ANTHROPIC_API_KEY"`) exists as a mechanism but is only used in the initial boot script template and by a small number of self-hosted users who configure env vars manually. In practice, nearly every instance has plaintext API keys in config.toml on the persistent volume.

This file is accessible via `GET /api/config/raw` in the dashboard and via `cat /data/config.toml` through the shell tool when sandbox is off. Users have leaked keys by screensharing their config page.

**Environment variables** on a live hosted instance are all non-sensitive infrastructure vars:

```
# Fly metadata (non-sensitive)
FLY_APP_NAME=sb-0759a0a6
FLY_REGION=iad
FLY_VM_MEMORY_MB=8192
FLY_IMAGE_REF=registry.fly.io/spacebot-image:v0.2.1

# Spacebot config (non-sensitive)
SPACEBOT_DEPLOYMENT=hosted
SPACEBOT_DIR=/data

# Standard process vars
HOME=/root
PATH=/data/tools/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
PWD=/data/agents/spacedrive-discord/workspace

# Browser config
CHROME_FLAGS=--no-sandbox --disable-dev-shm-usage --disable-gpu
```

API keys are NOT in the environment today — they're in config.toml. The secret store keeps it that way for system secrets and only exposes tool secrets as env vars.

**The existing secret store** (`src/secrets/store.rs`) implements AES-256-GCM encrypted key-value storage on redb with a `DecryptedSecret` wrapper that redacts in Debug/Display. It exists, is tested, but has zero callers in production.

## Problems

1. **Config is toxic to display.** The dashboard shows config.toml which contains literal API keys for nearly every user. Users have leaked keys by opening their config in screenshares or screenshots.

2. **Workers can read all secrets.** The config file is on disk at a known path (`/data/config.toml`). With sandbox off, `cat /data/config.toml` via the shell tool dumps every key. With sandbox on, the file is read-only but still readable.

3. **Agents can't safely inspect their own config.** The file tool blocks `/data/config.toml` (outside workspace), but workers have shell/exec which bypass that trivially. If keys were not in the config, agents could freely read it — useful for self-diagnosis ("what model am I configured to use?", "which messaging adapters are enabled?").

4. **No separation between internal and external secrets.** LLM API keys (needed only by the Rust process internally) and tool credentials (needed by CLI tools workers invoke) are stored and handled identically. There's no reason a worker subprocess should ever see `ANTHROPIC_API_KEY`.

5. **Prompt injection risk.** A malicious message in a Discord channel could attempt to convince the agent to read and output the config file. The leak detection hook catches known API key patterns, but if the key format isn't in the pattern list, it goes through.

## Design

### Two Secret Categories

The category controls **subprocess exposure**, not internal access. All secrets in the store are readable by Rust code via `SecretsStore::get()` regardless of category. The category answers one question: **should this value be injected as an env var into worker subprocesses?**

**System secrets** — not exposed to subprocesses. Rust code reads them from the store for internal use (LLM clients, messaging adapters, webhook integrations). Workers never see these as env vars.

Examples:

- `ANTHROPIC_API_KEY`
- `OPENAI_API_KEY`
- `DISCORD_BOT_TOKEN`
- `TELEGRAM_BOT_TOKEN`
- Slack tokens, webhook signing secrets

**Tool secrets** — exposed to subprocesses as env vars. Rust code can also read them from the store if needed (e.g., a `GITHUB_TOKEN` used by both a Rust webhook integration and by `gh` CLI in workers). `wrap()` injects these via `--setenv` (bubblewrap) or `Command::env()` (passthrough).

Examples:

- `GH_TOKEN` / `GITHUB_TOKEN`
- `NPM_TOKEN`
- `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`
- `DOCKER_TOKEN`
- Any user-configured credential their agent needs at the shell level

Tool is a superset of system in terms of access — tool secrets are readable by Rust code AND visible to workers. System secrets are readable by Rust code only. If a credential is needed by both Rust internals and CLI tools, make it a tool secret. There's no "both" category because tool already implies both.

### Category Assignment

The dashboard's secrets panel exposes the category when adding or editing a secret. Known keys are auto-categorized:

| Pattern                                   | Category       | Rationale                                                 |
| ----------------------------------------- | -------------- | --------------------------------------------------------- |
| `*_API_KEY` matching known LLM providers  | System         | Only LlmManager needs these                               |
| `DISCORD_BOT_TOKEN`, `TELEGRAM_BOT_TOKEN` | System         | Only MessagingManager needs these                         |
| `SLACK_*_TOKEN`, `SLACK_SIGNING_SECRET`   | System         | Only Slack adapter needs these                            |
| `GH_TOKEN`, `GITHUB_TOKEN`                | Tool           | `gh` CLI expects this                                     |
| `NPM_TOKEN`                               | Tool           | `npm` expects this                                        |
| `AWS_*`                                   | Tool           | AWS CLI expects these                                     |
| Everything else                           | **System (default)** | Unknown credentials default to the more restrictive category — not exposed to workers |

Users can override the auto-categorization. The default for unknown secrets is **system** (not exposed to workers) because defaulting to tool would be privilege-expanding — an internal credential accidentally categorized as tool becomes visible to every worker subprocess. It's safer to require the user to explicitly opt a secret into tool category if workers need it. The dashboard shows a clear prompt: *"Should worker processes have access to this credential? (Required for CLI tools like `gh`, `npm`, `aws`.)"*

### Two Modes: Unencrypted and Encrypted

The secret store operates in two modes:

**Unencrypted (default):** Secrets are stored in plaintext in `secrets.redb`. The store is always available — no master key needed, no unlock step, no setup. All secret store features work: `secret:` config references, system/tool categorization, env sanitization, output scrubbing, worker secret name injection. The only thing missing is encryption at rest — if someone gets access to the redb file, they can read the secrets.

This is still a significant improvement over the current state (plaintext keys in config.toml) because:
- Config.toml is safe to display (only `secret:` aliases).
- System secrets never enter subprocess environments.
- Tool secret values are scrubbed from worker output.
- The dashboard secrets panel never shows plaintext values.
- The attack surface narrows from "read a config file" to "read a specific redb file and know how to parse it."

**Encrypted (opt-in, recommended):** User enables encryption by generating a master key. Secrets are encrypted with AES-256-GCM in redb. The master key is stored in the OS credential store (macOS Keychain, Linux kernel keyring) — never as an env var or file on disk. Even if the volume is compromised, secrets are unreadable without the key.

Hosted instances are always encrypted — the platform generates and manages the master key automatically. Self-hosted users can enable encryption at any time via the dashboard or CLI.

### Master Key Storage (Encrypted Mode)

When encryption is enabled, the master key is stored in the **OS credential store** — macOS Keychain or Linux kernel keyring. It never exists as an environment variable or a file readable by workers. This is the only approach that definitively protects the key from LLM-driven workers regardless of sandbox state.

#### Why Not an Env Var or File?

Both are readable by workers when sandbox is off:

- **Env var:** `std::env::remove_var()` removes the variable from libc's environ list, but on Linux `/proc/self/environ` is an immutable kernel snapshot from exec time. A worker running `cat /proc/<pid>/environ | strings | grep MASTER` retrieves the key even after removal. On macOS, `ps eww <pid>` can show environment variables.
- **File on disk:** Without sandbox, the worker runs as the same user and can `cat /data/.master_key` or any other path. File permissions don't help — same user, same access.

With sandbox on, both can be protected (bubblewrap's `--unshare-pid` hides `/proc`, and the key file can be excluded from bind mounts). But the secret store should be secure regardless of sandbox state — sandbox protects the workspace, the OS credential store protects the master key. Independent layers.

#### macOS: Keychain

- Store via `SecItemAdd` / retrieve via `SecItemCopyMatching` using the Security framework.
- Access is controlled by the calling binary's code signature and ACL. A `bash` subprocess spawned by ShellTool is a different binary — Keychain will not grant access without explicit user authorization.
- Even `security find-generic-password` from a worker fails because the Keychain item's access list only includes the Spacebot binary.
- The `security-framework` crate provides safe Rust bindings.

**Keychain item:**
```
Service:  "sh.spacebot.master-key"
Account:  "<instance_id>" (or "default" for self-hosted single-instance)
Data:     <32 random bytes>
Access:   Spacebot binary only (kSecAttrAccessibleAfterFirstUnlock)
```

#### Linux: Kernel Keyring (`keyctl`)

- Store via `add_key("user", "spacebot_master", key_bytes, session_keyring)` — the key lives in kernel memory, not on any filesystem.
- Scoped to a **session keyring**. Spacebot creates a new session keyring on startup via `keyctl_join_session_keyring()`. Workers are spawned with a fresh empty session keyring via `pre_exec` — they cannot access the parent's keyring.
- No file to `cat`, no env var to read, no `/proc` exposure. The key is only accessible via the `keyctl` syscall with the correct keyring ID, which workers don't have.
- Works without sandbox, without root. The kernel enforces access control at the syscall level.

**Worker isolation (pre_exec):**
```rust
// Before exec'ing the worker subprocess:
unsafe {
    command.pre_exec(|| {
        // Give the child a new empty session keyring.
        // It cannot access the parent's session keyring.
        // CRITICAL: if this fails, the child inherits the parent's keyring
        // and can access the master key. Fail hard — do not spawn the worker.
        let result = libc::syscall(
            libc::SYS_keyctl,
            0x01, /* KEYCTL_JOIN_SESSION_KEYRING */
            std::ptr::null::<libc::c_char>(),
        );
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    });
}
```

If `KEYCTL_JOIN_SESSION_KEYRING` fails (returns -1), `pre_exec` returns `Err`, which causes `Command::spawn()` to fail. The worker is not started. This is the correct behavior — a worker that inherits the parent's session keyring could access the master key via `keyctl read`. The failure should be logged as an error with the errno for debugging (common causes: kernel compiled without `CONFIG_KEYS`, seccomp policy blocking `keyctl`).

This is additive to `--clearenv` — env sanitization strips env vars, the session keyring swap strips keyring access. Both run regardless of sandbox state.

#### Hosted Deployment

The platform generates a per-instance master key on provisioning and stores it in the platform database (tied to the user's account). On instance startup, the platform writes the key to a **tmpfs file** that Spacebot reads once, then the platform deletes it. Spacebot stores the key in the Linux kernel keyring immediately and never persists it to the volume.

Flow:
1. Platform provisions instance → generates 32-byte random key → stores in platform DB (`instances.master_key`, encrypted at rest).
2. Platform starts Fly machine → writes key to `/run/spacebot/master_key` (tmpfs, 0600, root-only).
3. Spacebot startup → reads `/run/spacebot/master_key` → stores in kernel keyring → deletes the tmpfs file.
4. Key is now only in kernel memory. Volume compromise doesn't expose it. `/run` is tmpfs, wiped on restart.
5. On next restart, the platform re-injects the key via the same tmpfs mechanism.

Properties:
- The key persists across restarts and rollouts — the platform always re-injects it from its database.
- The key is tied to the user's platform account, not to the volume. If the volume is compromised without platform access, the `secrets.redb` file is useless.
- The key is platform-managed. The user never sees the raw master key — the platform handles injection and rotation. The user can trigger rotation via the dashboard ("Rotate Encryption Key"), which calls the platform API to generate a new key, re-encrypt, and update its database. No key is displayed to the user.
- The platform database becomes a store of master keys, but it already stores auth tokens and Stripe credentials — same protection requirements.

#### Self-Hosted Deployment

The secret store is **always enabled** on self-hosted instances — it works in unencrypted mode out of the box. Users can opt into encryption through the dashboard:

1. User navigates to Settings → Secrets. The secrets panel is fully functional (add, edit, delete secrets). A banner shows: *"Secrets are stored without encryption. Enable encryption for protection against volume compromise."*
2. User clicks "Enable Encryption."
3. Spacebot generates a 32-byte random master key, encrypts all existing secrets in place.
4. The key is stored in the OS credential store (Keychain on macOS, kernel keyring on Linux).
5. The dashboard displays the key once: *"Save this master key somewhere safe. You'll need it to unlock the secret manager after a reboot (Linux) or if the Keychain is reset (macOS)."*
6. The key is **not written to disk**. No `.master_key` file, no env var, no config entry. The only durable copies are the OS credential store and whatever the user saved externally.

**On macOS**, the Keychain persists across restarts. The encrypted store unlocks automatically on every boot. The user's saved copy is a disaster-recovery backup only.

**On Linux**, the kernel keyring is cleared on reboot. After a reboot, the encrypted store starts in a **locked** state. The user unlocks it via the dashboard or CLI by providing the master key (see "Dashboard & CLI Secret Management" below). This is a deliberate trade-off:

- **No key file on disk** means no file for workers to `cat` — the master key is protected regardless of sandbox state. This was the entire motivation for the OS credential store design.
- **Unlock on reboot** is a one-time action per boot. For a headless bot that reboots rarely (planned maintenance, kernel updates), this is an acceptable cost.
- **All secrets are unavailable while locked.** `POST /api/secrets/encrypt` does an in-place encryption — plaintext values are wiped and replaced with encrypted versions. There is no parallel plaintext copy. When locked, `secret:` references fail to resolve, LLM providers and messaging adapters start without credentials (degraded mode). The bot starts and the control API is available (so the unlock command has something to talk to), but it cannot process messages or make LLM calls until unlocked.
- **Secrets configured via `env:` or literal values in config.toml still work while locked** — they don't go through the secret store. If a user keeps some credentials in `env:` config references as a fallback for critical services (e.g., the Discord bot token to stay connected and receive the unlock command), those work regardless of store state. This is a valid pattern for Linux self-hosters who want encryption but also want the bot to stay reachable after reboot.

For users who don't want the unlock-after-reboot requirement, the unencrypted store works indefinitely. All features except encryption at rest are available.

**Startup flow (all deployments):**

1. Open `SecretsStore` (redb). The store always initializes — it works in unencrypted mode by default.
2. Check if the store contains encrypted secrets (encryption header in redb metadata).
3. If **unencrypted** (or no secrets yet): store is immediately available. Read all secrets. Skip to step 7.
4. If **encrypted**: check OS credential store for master key (Keychain / kernel keyring).
5. If master key found (or injected via tmpfs on hosted → load into OS credential store → delete tmpfs file): derive AES-256-GCM cipher key via Argon2id (~100ms), decrypt all secrets. Store is **unlocked**.
6. If master key not found: store enters **locked** state. Encrypted secrets are unavailable. **The bot still starts** — the control API (port 19898) and embedded dashboard come up so the user can issue the unlock command. LLM providers and messaging adapters that depend on `secret:` references start in degraded mode (no credentials). Components configured via `env:` or literal config values still work. On unlock (via dashboard or CLI): key is loaded into OS credential store → derive cipher → decrypt → re-initialize dependent components.
7. **System secrets:** passed directly to Rust components (`LlmManager`, `MessagingManager`, etc.). Never set as env vars.
8. **Tool secrets:** held in a `HashMap<String, String>` for `Sandbox::wrap()` injection via `--setenv`.

#### Key Derivation

Argon2id rather than the current SHA-256 in `build_cipher()`. For hosted instances where the platform generates a random high-entropy key, SHA-256 would be fine — but self-hosted users may use a passphrase (future: dashboard "set your own key" option), and SHA-256 of a passphrase is trivially brutable. Argon2id handles both cases correctly (memory-hard, resistant to GPU/ASIC attacks) and the cost is a one-time ~100ms at startup. No reason to ship the weak path and upgrade later.

#### Upgrade Path

1. **New version ships.** The secret store is enabled by default in unencrypted mode. Auto-migration runs on first boot: literal keys in config.toml → secret store (unencrypted), config.toml rewritten with `secret:` references. All security features work immediately (env sanitization, system/tool separation, output scrubbing, safe config display).
2. **Hosted users:** The platform also generates master keys for all instances and injects them via tmpfs. Migration encrypts secrets automatically. Fully encrypted from day one, no user action required.
3. **Self-hosted users:** Migration to the unencrypted store is automatic. The dashboard shows a banner encouraging encryption: *"Secrets are stored without encryption. Enable encryption for protection against volume compromise."* Encryption is opt-in — user clicks "Enable Encryption" when ready.

On startup, Spacebot scans the environment for variables matching known secret patterns (anything with `TOKEN`, `KEY`, `SECRET`, `PASSWORD` in the name). If found and `passthrough_env` doesn't list them, a prominent warning is logged: "Detected secrets in environment variables. Consider moving them to the secret store." The warning is informational — nothing breaks, nothing is stripped.

### Config Resolution Prefixes

Config values support three resolution modes via prefix:

```toml
# Literal — plaintext value inline (current default, what migration replaces)
anthropic_key = "sk-ant-abc123..."

# env: — read from system environment variable at resolve time
anthropic_key = "env:ANTHROPIC_API_KEY"

# secret: — read from secret store (held in memory)
anthropic_key = "secret:ANTHROPIC_API_KEY"
```

The `secret:` prefix is a resolution directive: "this value lives in the `SecretsStore`, look it up by this name, return the value." The config doesn't know or care whether the secret is categorized as system or tool, or whether the store is encrypted — it just gets the resolved string. The category is metadata on the secret in the store and only matters at `wrap()` time when deciding what to inject into worker subprocesses.

**Secret names use UPPER_SNAKE_CASE matching env var convention.** The secret name in the store IS the env var name — `GH_TOKEN`, not `gh_token`. For tool secrets, `wrap()` iterates the store and does `--setenv {name} {value}` with no translation. For system secrets, the name is just an identifier (they're never env vars), but using the same convention keeps everything consistent. Skills that say "set `GH_TOKEN`" map directly to a secret named `GH_TOKEN` in the dashboard.

### Config Key Migration

All provider keys and sensitive tokens move from config.toml to the secret store. Config.toml changes from:

```toml
[llm]
anthropic_key = "sk-ant-abc123..."

[messaging.discord]
token = "env:DISCORD_BOT_TOKEN"
```

To:

```toml
[llm]
anthropic_key = "secret:ANTHROPIC_API_KEY"

[messaging.discord]
token = "secret:DISCORD_BOT_TOKEN"
```

The `resolve_env_value()` function (`config.rs:2974`) is extended to handle the `secret:` prefix:

```rust
fn resolve_secret_or_env(value: &str, secrets: &SecretsStore) -> Option<String> {
    if let Some(alias) = value.strip_prefix("secret:") {
        secrets.get(alias).ok().map(|s| s.expose().to_string())
    } else if let Some(var_name) = value.strip_prefix("env:") {
        std::env::var(var_name).ok()
    } else {
        Some(value.to_string()) // literal
    }
}
```

The resolved values are consumed by Rust code (provider constructors, adapter init). They are never set as env vars. The `secret:` prefix is the config-level reference; the category (system vs tool) determines runtime behavior.

**Migration path:**

1. On first boot with the new version, if config.toml contains literal key values (not `env:` or `secret:` prefixed), auto-migrate: store each literal value in the secret store under a deterministic UPPER_SNAKE_CASE name (e.g., `anthropic_key` → `secret:ANTHROPIC_API_KEY`), with auto-detected category. Encryption is not required — migration works in unencrypted mode.
2. Rewrite config.toml in place to replace literal values with `secret:` references.
3. Log every migration step. If migration fails for any key, leave the original value in config.toml and warn.
4. For `env:` prefixed values, leave them as-is. They're already not storing the secret in the config. Users who want to migrate `env:` values to the secret store can do so explicitly via the dashboard.
5. The `env:` prefix continues to work for users who prefer env-var-based key management.
6. **Hosted:** Migration runs automatically on first boot. The platform also enables encryption (see Upgrade Path above).
7. **Self-hosted:** Migration runs automatically on first boot. The store starts in unencrypted mode. Users can enable encryption later via the dashboard.

### Dashboard Changes

- **Provider setup** writes `secret:` references by default. The "API Key" field in the provider UI is a password input that sends the value to the API, which stores it in the secret store (as a system secret) and writes `secret:provider_name` to config.toml.
- **Raw config view** (`GET /api/config/raw`) is safe to display since config.toml only contains aliases.
- **Secrets panel** — list all secrets with name, category (system/tool), and masked value. Add/remove/rotate. Category is editable. Never displays plaintext values (shows masked `***` with a copy button that copies from a short-lived in-memory decryption).
- **Secret store status** — indicator showing store state: `unencrypted` (working, encryption available), `unlocked` (encrypted and operational), or `locked` (encrypted, needs master key). For hosted instances, always `unlocked` (platform-managed). See "Dashboard & CLI Secret Management" for full UX details.

### Env Sanitization Integration

The sandbox `wrap()` function (see sandbox-hardening.md, Section 2) handles the env var injection:

1. `--clearenv` strips everything from the subprocess.
2. Re-add safe vars: `PATH` (with tools/bin), `HOME`, `USER`, `LANG`, `TERM`.
3. Re-add **tool secrets only** — iterate the secret store's tool category, `--setenv` each into the subprocess.
4. System secrets are **never** injected. The master key is never in the process environment (it lives in the OS credential store), so there's nothing to strip — but even if it were, `--clearenv` would exclude it.

The `Sandbox` struct needs access to the tool secrets. Options:

- `Sandbox` holds an `Arc<ArcSwap<HashMap<String, String>>>` of tool env vars, updated when secrets change.
- The `SecretsStore` exposes a `tool_env_vars() -> HashMap<String, String>` method, and `Sandbox` holds an `Arc<SecretsStore>`.

Either way, `wrap()` reads the current tool secrets on each call and injects them. This is cheap — the set changes rarely (only when the user adds/removes secrets via the dashboard).

### Worker Secret Awareness

Workers get the **names** of available tool secrets injected into their system prompt — never the values. This tells the worker what credentials are available without it having to run `printenv` to discover them:

```
Available tool secrets (set as environment variables in your shell):
  GH_TOKEN, NPM_TOKEN, AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY

Commands that use these credentials will work automatically (e.g., gh commands
use GH_TOKEN). Do not echo, print, or log secret values.
```

This is assembled at worker construction time from the tool secret names in the store. The list updates when secrets change (workers spawned after a secret is added/removed get the updated list).

**Why this matters:**
- Without it, the worker has to guess or run `printenv` to find out what's available. That wastes a turn and the `printenv` output contains the actual values in the worker's own context (even though the scrubber would catch them before they reach the channel).
- With it, the worker knows `GH_TOKEN` is available and can use `gh` commands immediately. No discovery step, no secret values in context.
- Skills that say "requires `GH_TOKEN`" align directly — the worker sees the name in its prompt and knows the credential is present.
- The "do not print secret values" instruction is a soft guardrail. The real guardrail is the output scrubber. But telling the LLM not to do it reduces the frequency, which means less scrubbing and less noise in logs.

### Output Scrubbing (Tool Secret Redaction)

Workers need tool secrets in their subprocess environment to run CLI tools. But there's no reason the secret _values_ should propagate back up to channels or branches. A worker running `gh pr create` needs `GH_TOKEN` in its env, but the channel receiving the worker's result doesn't need to see the token value if it leaks into stdout.

**Mechanism:** Every string that flows from a worker back toward a channel or branch is checked against the current set of tool secret values (exact substring match). Any match is replaced with `[REDACTED:<secret_name>]`.

```
Worker stdout: "Authenticated as user X. Token: ghp_abc123def456..."
Scrubber: checks against all tool_env_vars() values
Match found: "ghp_abc123def456..." == tool secret "GH_TOKEN"
Redacted:    "Authenticated as user X. Token: [REDACTED:GH_TOKEN]"
```

**Where it runs:**

| Output path              | Scrubbing point                                                    |
| ------------------------ | ------------------------------------------------------------------ |
| Worker result text       | Before injection into channel/branch history                       |
| `set_status` tool output | Before adding to the status block (channels read this every turn)  |
| OpenCode SSE events      | Before forwarding to the worker event handler                      |
| Branch conclusions       | Before injection into channel history (branches can spawn workers) |

**Why exact match, not regex:** Leak detection (SpacebotHook) already does regex pattern matching for known key formats (`sk-ant-*`, `ghp_*`, etc.). The scrubber is complementary — it catches secrets that don't have recognizable patterns. A random 64-char string the user stored as a tool secret has no regex pattern, but the scrubber knows its exact value and catches it. The two layers work together:

- **Leak detection (regex):** catches known formats even if the secret isn't in the store (e.g., a key the user typed inline). Reactive — kills the agent after detection.
- **Output scrubbing (exact match):** catches any stored tool secret regardless of format. Proactive — redacts before the value reaches the channel. The channel sees `[REDACTED:GH_TOKEN]` and knows the secret was used, but never sees the value.

**Streaming safety (split-secret problem):** If a secret value is split across two adjacent SSE events or stream chunks, a per-string exact match misses it. For example, `GH_TOKEN = "ghp_abc123def456"` split as `"...ghp_abc123"` + `"def456..."` — neither chunk contains the full secret.

Fix: the scrubber maintains a **rolling buffer** per output stream. For each stream (identified by worker ID + output path), the scrubber holds the tail of the previous chunk, sized to `max_secret_length - 1` bytes. On each new chunk, it concatenates `[tail_of_previous | new_chunk]`, scrubs the combined string, then emits everything except the new tail (which is held for the next chunk). On stream end, the held tail is flushed and scrubbed.

```rust
struct StreamScrubber {
    buffer: String,
    max_secret_len: usize, // max length across all tool secret values
}

impl StreamScrubber {
    fn scrub_chunk(&mut self, chunk: &str, secrets: &HashMap<String, String>) -> String {
        self.buffer.push_str(chunk);
        let emit_up_to = self.buffer.len().saturating_sub(self.max_secret_len - 1);
        let to_emit = scrub_secrets(&self.buffer[..emit_up_to], secrets);
        self.buffer = self.buffer[emit_up_to..].to_string();
        to_emit
    }

    fn flush(&mut self, secrets: &HashMap<String, String>) -> String {
        let remaining = std::mem::take(&mut self.buffer);
        scrub_secrets(&remaining, secrets)
    }
}
```

This adds latency of `max_secret_len` bytes per chunk — typically 40-100 bytes for API keys. Negligible for worker output which is displayed progressively anyway.

For non-streaming paths (worker result text, branch conclusions), the full string is available at once — no buffer needed, simple `scrub_secrets()` call.

**Cost:** Comparing every worker output string against every tool secret value. With typically <20 secrets and output in the KB range, this is a substring search over a small set — negligible. The secret values are already in memory (the tool env var cache). No decryption on each check.

**Implementation:** A `scrub_secrets(text: &str, tool_secrets: &HashMap<String, String>) -> String` function that iterates the map and replaces all occurrences, plus `StreamScrubber` for chunked output paths (OpenCode SSE, streaming tool output). Both live in `src/secrets/scrub.rs`.

### Protection Layers (Summary)

| Layer                                                  | Requires Encryption | What It Protects Against                                                       |
| ------------------------------------------------------ | ------------------- | ------------------------------------------------------------------------------ |
| `secret:` aliases in config.toml                       | No                  | Config file exposure (screenshare, `cat`, dashboard display)                   |
| System/tool category separation                        | No                  | Workers seeing LLM API keys, messaging tokens, or other internal credentials   |
| `DecryptedSecret` wrapper                              | No                  | Accidental logging of secret values in tracing output                          |
| Env sanitization (`--clearenv` + selective `--setenv`) | No                  | Workers only get tool secrets, never system secrets or internal vars |
| Worker secret name injection (prompt)                  | No                  | Workers know what credentials are available without running `printenv` — no secret values in LLM context |
| Output scrubbing (exact match)                         | No                  | Tool secret values propagating from worker output back to channels/branches |
| Leak detection (SpacebotHook, regex)                   | No                  | Last-resort safety net — known key format patterns in any tool output          |
| Secret store encryption (AES-256-GCM)                  | **Yes**             | Disk access to secrets.redb (stolen volume, backup leak)                       |
| Master key in OS credential store (Keychain / kernel keyring) | **Yes**       | Worker access to the encryption key — OS enforces access control at the binary/keyring level, independent of sandbox state |
| Worker session keyring isolation (`pre_exec`)          | **Yes**             | Workers accessing parent's kernel keyring on Linux (additive to `--clearenv`)  |

### What This Doesn't Solve

- **Workers can see tool secrets in their own process.** A worker running `printenv GH_TOKEN` gets the value in its subprocess. This is by design — the worker needs it to run `gh` commands. But the value is scrubbed from the worker's output before it reaches the channel.
- **Side channels within the workspace.** A worker could write a tool secret to a file, then a subsequent worker reads it. The sandbox limits where files can be written, but within the workspace it's unrestricted. Output scrubbing catches the value if it appears in any worker's output, but not if it stays in a file.
- **Encoding/obfuscation.** A worker could base64-encode a secret value before outputting it. The exact-match scrubber wouldn't catch the encoded form. Leak detection's regex patterns also wouldn't match. This is a theoretical attack by a deliberately adversarial LLM, not an accidental leak.

The key properties: **system secrets never leave Rust memory.** Tool secrets reach worker subprocesses (necessary) but are scrubbed from all output flowing back to channels (the LLM context). A channel never sees a secret value — it sees `[REDACTED:GH_TOKEN]` at most.

## Files Changed

| File                                  | Change                                                                                                                                                              |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/secrets/store.rs`                | Add unencrypted mode; add secret category (system/tool); add `tool_env_vars()` method; add `tool_secret_names()` for prompt injection; encrypt-in-place support; Argon2id key derivation for encrypted mode |
| `src/secrets/scrub.rs`                | New: `scrub_secrets()` — exact-match redaction of tool secret values in output strings                                                                              |
| `src/config.rs`                       | Extend `resolve_env_value()` to handle `secret:` prefix; wire `SecretsStore` into config loading; migration logic for existing literal/env keys                     |
| `src/sandbox.rs`                      | `wrap()` injects tool secrets via `--setenv`; holds reference to secret store or tool env var cache                                                                 |
| `src/agent/worker.rs`                 | Scrub worker result text through `scrub_secrets()` before injecting into channel/branch history; inject tool secret names into worker system prompt                  |
| `src/tools/set_status.rs`             | Scrub status text through `scrub_secrets()` before updating status block                                                                                            |
| `src/opencode/worker.rs`              | Scrub OpenCode SSE output events through `scrub_secrets()` before forwarding                                                                                        |
| `src/api/secrets.rs`                  | New: secret CRUD, status, encrypt, unlock/lock, rotate, migrate, export/import endpoints                                                                           |
| `src/api/server.rs`                   | Add secret management routes                                                                                                                                        |
| `src/secrets/keystore.rs`             | New: OS credential store abstraction — `KeyStore` trait with `MacOSKeyStore` (Security framework / Keychain) and `LinuxKeyStore` (kernel keyring / keyctl) backends  |
| `src/main.rs`                         | Initialize `SecretsStore` (unencrypted or encrypted), auto-migrate literal keys from config.toml on first boot, load master key from OS credential store if encrypted (or enter locked state), pass system secrets to provider init |
| `src/agent/worker.rs`                 | Add `pre_exec` hook to spawn workers with a fresh empty session keyring (Linux) |
| `spacebot-platform/api/src/fly.rs`    | Generate per-instance master key on provisioning, store in platform DB, inject via tmpfs at `/run/spacebot/master_key` in `machine_config()`                        |
| `spacebot-platform/api/src/db.rs`     | Add `master_key` column to instances table (encrypted at rest)                                                                                                      |
| `spacebot-platform/api/src/routes.rs` | Add master key rotation endpoint for dashboard                                                                                                                      |

## Phase Plan

**Hard dependency on sandbox-hardening.md Phase 2 (env sanitization).** The master key is protected independently by the OS credential store, but without `--clearenv`, system secrets and other env vars still leak to worker subprocesses.

### Phase 1: Core Secret Store (Unencrypted)

The secret store ships and works immediately for all users without any setup.

1. Extend `SecretsStore` to support unencrypted mode (plaintext values in redb, no cipher needed).
2. Extend `resolve_env_value()` to handle `secret:` prefix.
3. Auto-migration on first boot: detect literal keys in config.toml → store in redb → rewrite config.toml with `secret:` references.
4. System secrets: pass values directly to `LlmManager`, `MessagingManager`, etc. during init. Never set as env vars.
5. Tool secrets: expose via `tool_env_vars()` for `Sandbox::wrap()` injection.
6. Inject tool secret names (not values) into worker system prompts at construction time.
7. Secret CRUD API: `GET/PUT/DELETE /api/secrets/:name`, `GET /api/secrets/status`.
8. Dashboard secrets panel: list, add, edit, delete secrets with category assignment.

### Phase 1.5: Encryption (Opt-In)

Layered on top of the working unencrypted store.

1. Implement `KeyStore` abstraction with macOS Keychain and Linux kernel keyring backends (`src/secrets/keystore.rs`).
2. `POST /api/secrets/encrypt` — generate master key, encrypt all existing secrets in place, store key in OS credential store.
3. Add `pre_exec` hook to worker spawning: children get a fresh empty session keyring (Linux).
4. Startup: detect encrypted store → load master key from OS credential store → derive cipher key via Argon2id → decrypt. If key not found → locked state.
5. Unlock/lock API: `POST /api/secrets/unlock`, `POST /api/secrets/lock`.
6. Platform: generate per-instance master key on provisioning, store in platform DB, inject via tmpfs at `/run/spacebot/master_key`. Hosted instances are always encrypted.
7. Key rotation: `POST /api/secrets/rotate`.
8. Export/import for backup and migration.

### Phase 2: Output Scrubbing

1. Implement `scrub_secrets()` — exact substring match against all tool secret values, replace with `[REDACTED:<name>]`.
2. Wire into worker result path (before channel/branch history injection).
3. Wire into `set_status` (before status block update).
4. Wire into OpenCode SSE event forwarding.
5. Wire into branch conclusion path (before channel history injection).
6. Verify: worker running `echo $GH_TOKEN` produces `[REDACTED:GH_TOKEN]` in the channel's view of the result.

### Phase 3: Hosted Encryption Rollout

1. Platform generates master keys for all existing instances, stores in platform DB.
2. On next image rollout, keys are injected via tmpfs. Encryption is enabled automatically on first boot with the new image.
3. Verify: all hosted instances have encrypted stores; config.toml contains no plaintext keys; `GET /api/config/raw` is safe to display.

### Phase 4: Dashboard & CLI

1. Secrets panel: list secrets with name, category (system/tool), masked value. Add/remove/rotate. Works in both unencrypted and unlocked states.
2. Provider setup UI writes `secret:` references and stores as system secrets.
3. Secret store status indicator (`unencrypted` / `locked` / `unlocked`).
4. Encryption onboarding banner for unencrypted self-hosted stores.
5. Unlock prompt for locked stores (after Linux reboot).
6. CLI `spacebot secrets` subcommand tree. See "Dashboard & CLI Secret Management" for full specification.
7. Hosted: master key rotation via platform API.

## Open Questions

1. **Secret rotation and hot-reload.** When a user rotates a key (e.g., regenerates their Anthropic API key), the workflow is: update via dashboard → store encrypts new value → config references unchanged. But the LLM manager holds the old key in memory. Do we need a reload hook that re-reads system secrets from the store? Tool secrets are re-read on each `wrap()` call so they update automatically.
2. **Migration rollback.** After migrating keys from config.toml to the secret store, if the secret store becomes corrupted or the master key is lost, is there a recovery path? Should we keep a one-time encrypted backup of the pre-migration config?
3. **Platform master key storage.** The platform database will store per-instance master keys. What's the encryption/protection model for the platform database itself? Should the platform encrypt master keys at rest with its own key?
4. **Category override UX.** How prominent should the system/tool toggle be in the dashboard? Auto-categorization handles the common cases, but users need to understand the distinction to make informed overrides.
5. ~~**Self-hosted tool secrets without master key.**~~ **Resolved — see `passthrough_env` below.**
6. ~~**Linux key file exposure without sandbox.**~~ **Resolved — no key file on disk.** Linux uses the kernel keyring only. After reboot, the secret store enters a locked state and the user unlocks via dashboard or CLI. See "Dashboard & CLI Secret Management" below.
7. **Keychain ACL on unsigned dev builds.** During development, the Spacebot binary may not be code-signed. macOS Keychain ACLs based on code signature won't work for unsigned binaries — need to handle this gracefully (fall back to file-based storage in dev mode, or use a less restrictive Keychain access policy).

### Env Passthrough for Self-Hosted

Some self-hosted users set credentials as env vars in Docker compose or systemd rather than through the dashboard. With env sanitization (`--clearenv`), these env vars get stripped from worker subprocesses. The secret store handles this for secrets it knows about (they're injected via `--setenv`), but env vars that haven't been migrated to the store would be silently lost. That's a breaking change.

**Fix:** A configurable passthrough list in the sandbox config:

```toml
[agents.sandbox]
mode = "enabled"
passthrough_env = ["GH_TOKEN", "GITHUB_TOKEN", "NPM_TOKEN"]
```

`wrap()` builds the subprocess environment from three sources, checked in order:

1. **Safe vars** — always passed: `PATH` (with tools/bin), `HOME`, `USER`, `LANG`, `TERM`.
2. **Tool secrets from the store** — all tool-category secrets are injected via `--setenv` (works in both unencrypted and encrypted mode).
3. **`passthrough_env` list** — for each name in the list, if the var exists in the parent process environment, pass it through to the subprocess. This is the escape hatch for self-hosted users without a master key.

When secrets have been migrated to the store, `passthrough_env` is redundant for those vars. The config field still works (it's additive), but the dashboard can show a hint: "You have passthrough env vars configured. Consider moving these to the secret store."

On hosted instances, `passthrough_env` is empty by default and has no effect — the platform manages all secrets via the store.

**Why not just skip `--clearenv` when there's no master key?** Because `--clearenv` protects more than just the master key — it prevents system secrets, internal vars (`SPACEBOT_*`), and any other env vars from leaking to workers. The master key is protected by the OS credential store regardless of `--clearenv`, but env sanitization is still necessary for everything else. The passthrough list is explicit — the user declares exactly which vars they want forwarded. Everything else is stripped.

---

## Dashboard & CLI Secret Management

The secret store needs a complete management interface — onboarding, unlock/lock lifecycle, secret CRUD, and key backup/rotation. Both the embedded dashboard (SPA) and the CLI provide access to the same underlying API.

### Secret Store States

The store has three states, exposed via `GET /api/secrets/status`:

| State | Meaning | Dashboard Display |
|-------|---------|-------------------|
| **`unencrypted`** | Store is active, secrets stored in plaintext in redb. No master key configured. | Full secrets panel + banner: "Enable encryption for protection against volume compromise" |
| **`locked`** | Encryption is enabled but master key is not currently in the OS credential store. Happens after Linux reboot. | Unlock prompt: "Enter your master key to unlock encrypted secrets" + limited panel (can see secret names but not add/edit/read) |
| **`unlocked`** | Encryption is enabled and master key is in the OS credential store. Secrets are decrypted and operational. | Full secrets panel |

```json
// GET /api/secrets/status
{
  "state": "unencrypted",      // "unencrypted" | "locked" | "unlocked"
  "encrypted": false,          // whether encryption is enabled
  "secret_count": 12,          // total secrets in store
  "system_count": 5,           // system category count
  "tool_count": 7,             // tool category count
  "platform_managed": false    // true on hosted (encryption is automatic, UI hides encryption controls)
}
```

How to distinguish states: the redb metadata contains an `encrypted` flag. If `encrypted == false` → `unencrypted`. If `encrypted == true` and master key is in OS credential store → `unlocked`. If `encrypted == true` and master key not found → `locked`.

### Enabling Encryption (Self-Hosted)

The secret store works immediately without encryption. Enabling encryption is a separate step.

**Dashboard:**

1. User navigates to Settings → Secrets. The secrets panel is fully functional. A banner shows: *"Secrets are stored without encryption. Enable encryption for protection against volume compromise."*
2. Clicks "Enable Encryption."
3. `POST /api/secrets/encrypt` — Spacebot generates a 32-byte random key, stores it in the OS credential store, encrypts all existing secrets in place, returns the key as a hex string.
4. Dashboard displays the key in a modal with a copy button and a warning: *"Save this key somewhere safe. On Linux, you'll need it to unlock the secret manager after a reboot. This is the only time the key will be shown."*
5. User confirms they've saved the key. Banner disappears. Store is now encrypted.

**CLI:**

```bash
# Enable encryption
spacebot secrets encrypt
# Output:
# Encrypting 12 secrets...
# Master key: a1b2c3d4e5f6...  (64 hex chars)
#
# IMPORTANT: Save this key. You will need it to unlock the
# secret manager after a reboot. This is the only time it
# will be displayed.

# Migration (separate from encryption — runs automatically on first boot,
# or manually if needed)
spacebot secrets migrate
# Output:
# Detected 4 plaintext keys in config.toml:
#   anthropic_key     → ANTHROPIC_API_KEY (system)
#   openai_key        → OPENAI_API_KEY (system)
#   discord.token     → DISCORD_BOT_TOKEN (system)
#   github_token      → GH_TOKEN (tool)
# Migrate? [y/N]: y
# Migrated 4 keys. config.toml updated.
```

### Unlock / Lock Flow (Encrypted Mode Only)

Only applies when encryption is enabled. Unencrypted stores are always available.

**Dashboard:**

1. On page load, dashboard checks `GET /api/secrets/status`.
2. If `locked`: shows unlock card with a password input: *"Encrypted secrets are locked. Enter your master key to unlock."*
3. User pastes key → `POST /api/secrets/unlock` with `{ "master_key": "<hex>" }`.
4. Server validates the key (attempts to derive cipher and decrypt a known sentinel value in the store). If valid: loads into OS credential store, decrypts all secrets, re-initializes LLM providers and messaging adapters that were started without their secrets. Returns `200`.
5. If invalid: returns `401` with *"Invalid master key."* Dashboard shows error, lets user retry.
6. On success, dashboard transitions to the full secrets panel.

**CLI:**

```bash
# Unlock
spacebot secrets unlock
# Enter master key: ********
# Secret manager unlocked. 12 secrets decrypted.
# Re-initialized: LlmManager (3 providers), MessagingManager (2 adapters).

# Unlock non-interactively (for automation — key in stdin)
# NOTE: This example uses an env var for illustration. For production
# automation, pipe from a secrets manager or file, not a shell env var
# (env vars are visible in /proc and process listings — the same exposure
# the OS credential store is designed to avoid).
cat /run/secrets/spacebot_key | spacebot secrets unlock --stdin

# Lock (clears key from OS credential store — useful for maintenance)
spacebot secrets lock
# Secret manager locked. Secrets remain encrypted on disk.
# The bot will continue running with cached credentials until restart.

# Check status
spacebot secrets status
# State: unlocked
# Secrets: 12 (5 system, 7 tool)
```

**Lock behavior:** `POST /api/secrets/lock` removes the master key from the OS credential store. The derived cipher key in memory is zeroed. New secret operations fail. Already-decrypted values held by LLM providers and messaging adapters continue working until the process restarts — we don't forcefully kill active connections. On next restart, the store comes up locked.

### Secret CRUD

Available when the store is `unencrypted` or `unlocked`. When `locked`, read-only endpoints (`GET /api/secrets`, `GET /api/secrets/:name/info`) still work — secret names, categories, and metadata are stored as unencrypted headers in redb. Mutation endpoints (`PUT`, `DELETE`) and any operation that touches secret values return `423 Locked`.

**List secrets:**

```
GET /api/secrets
```
```json
{
  "secrets": [
    {
      "name": "ANTHROPIC_API_KEY",
      "category": "system",
      "created_at": "2026-02-25T10:30:00Z",
      "updated_at": "2026-02-25T10:30:00Z"
    },
    {
      "name": "GH_TOKEN",
      "category": "tool",
      "created_at": "2026-02-25T10:31:00Z",
      "updated_at": "2026-02-27T14:00:00Z"
    }
  ]
}
```

Values are never returned in list responses. Names and categories only.

**Add / update a secret:**

```
PUT /api/secrets/:name
```
```json
{
  "value": "ghp_abc123...",
  "category": "tool"
}
```

If the secret already exists, its value and/or category are updated. The `updated_at` timestamp is refreshed. For tool secrets, the change is immediately available to the next `wrap()` call — no restart needed.

For system secrets, the change is stored but active components (LLM providers, adapters) continue using the old value until a config reload or restart. The response indicates this:

```json
{
  "name": "ANTHROPIC_API_KEY",
  "category": "system",
  "reload_required": true,
  "message": "Secret updated. Reload config or restart for the new value to take effect."
}
```

**Delete a secret:**

```
DELETE /api/secrets/:name
```

Removes from the store. If the secret is referenced by config.toml (`secret:NAME`), the config reference becomes a dangling pointer — `resolve_secret_or_env` returns `None` and the component logs a warning. The response warns about this:

```json
{
  "deleted": "GH_TOKEN",
  "config_references": ["agents.tools.github_token"],
  "warning": "This secret is referenced in config.toml. The reference will fail to resolve."
}
```

**CLI equivalents:**

```bash
# List
spacebot secrets list
# NAME                  CATEGORY   UPDATED
# ANTHROPIC_API_KEY     system     2026-02-25 10:30
# GH_TOKEN              tool       2026-02-27 14:00

# Add/update
spacebot secrets set GH_TOKEN --category tool
# Enter value: ********
# Secret GH_TOKEN saved (tool).

# Or non-interactively
echo "ghp_abc123" | spacebot secrets set GH_TOKEN --category tool --stdin

# Delete
spacebot secrets delete GH_TOKEN
# Warning: GH_TOKEN is referenced in config.toml at agents.tools.github_token
# Delete anyway? [y/N]: y
# Deleted GH_TOKEN.

# Show category info
spacebot secrets info GH_TOKEN
# Name:     GH_TOKEN
# Category: tool
# Created:  2026-02-25 10:31
# Updated:  2026-02-27 14:00
# Config:   agents.tools.github_token = "secret:GH_TOKEN"
```

### Key Rotation

Master key rotation replaces the encryption key without changing the stored secrets:

1. User initiates rotation via dashboard or CLI.
2. `POST /api/secrets/rotate` — Spacebot generates a new master key, re-encrypts all secrets with the new key, stores the new key in the OS credential store, returns the new key for the user to save.
3. The old key is invalidated. The user's previously saved key no longer works for unlock.

**Dashboard:** Settings → Secrets → "Rotate Master Key" button. Confirms with a warning that the old key becomes invalid. Shows the new key in a modal.

**CLI:**

```bash
spacebot secrets rotate
# WARNING: This will invalidate your current master key.
# You will need to save the new key for future unlocks.
# Continue? [y/N]: y
#
# New master key: f7e8d9c0b1a2...
# Re-encrypted 12 secrets.
#
# IMPORTANT: Save this new key. Your old key no longer works.
```

**Hosted:** Key rotation is handled by the platform. The user clicks "Rotate Key" in the dashboard, which calls the platform API. The platform generates a new key, updates its database, re-injects on next restart. The user never sees the key.

### Key Export / Import

For disaster recovery and instance migration:

```bash
# Export all secrets (encrypted with the current master key)
spacebot secrets export --output secrets-backup.enc
# Exported 12 secrets to secrets-backup.enc
# This file is encrypted with your current master key.

# Import secrets from a backup
spacebot secrets import --input secrets-backup.enc
# Enter the master key used to create this backup: ********
# Imported 12 secrets. 3 conflicts (existing secrets with same name):
#   ANTHROPIC_API_KEY — kept existing (use --overwrite to replace)
#   GH_TOKEN — kept existing
#   NPM_TOKEN — kept existing

# Import with overwrite
spacebot secrets import --input secrets-backup.enc --overwrite
```

**Unencrypted store warning:** If encryption is not enabled, the export file contains plaintext secrets. The CLI warns:

```
# WARNING: Encryption is not enabled. This export contains
# plaintext secrets. Store it securely or enable encryption
# first with: spacebot secrets encrypt
```

When encryption is enabled, the export file is the raw encrypted redb data plus a header with the Argon2id salt. It's useless without the master key. This covers:
- **Backup before migration** — export before upgrading, import if something goes wrong.
- **Instance migration** — export from old instance, import into new instance with the same master key.
- **Disaster recovery** — if `secrets.redb` is corrupted, import from backup.

### API Summary

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/api/secrets/status` | GET | Token | Store state (`unencrypted` / `locked` / `unlocked`), secret counts |
| `/api/secrets` | GET | Token | List all secrets (name + category, no values) |
| `/api/secrets/:name` | PUT | Token | Add or update a secret |
| `/api/secrets/:name` | DELETE | Token | Delete a secret |
| `/api/secrets/:name/info` | GET | Token | Secret metadata + config references |
| `/api/secrets/migrate` | POST | Token | Auto-migrate literal keys from config.toml (runs automatically on first boot, manual trigger if needed) |
| `/api/secrets/encrypt` | POST | Token | Enable encryption: generate master key, encrypt all secrets, store key in OS credential store (only when `unencrypted`) |
| `/api/secrets/unlock` | POST | Token | Provide master key, decrypt store (only when `locked`) |
| `/api/secrets/lock` | POST | Token | Clear master key from OS credential store (only when `unlocked`) |
| `/api/secrets/rotate` | POST | Token | Rotate master key, re-encrypt all secrets (only when `unlocked`) |
| `/api/secrets/export` | POST | Token | Export backup (encrypted if encryption enabled, plaintext otherwise) |
| `/api/secrets/import` | POST | Token | Import from backup |

Read-only endpoints (`GET /api/secrets`, `GET /api/secrets/:name/info`, `GET /api/secrets/status`) work in all states — secret names and categories are unencrypted metadata. Mutation endpoints (`PUT /api/secrets/:name`, `DELETE /api/secrets/:name`) work when `unencrypted` or `unlocked` but return `423 Locked` when locked (can't encrypt new values without the key). The encryption/unlock/lock/rotate endpoints only apply to encrypted stores.

Authentication uses the same bearer token as the rest of the control API. On hosted instances, the dashboard proxy handles auth transparently. Self-hosted users authenticate via their configured API token.

### CLI Subcommand Structure

```
spacebot secrets
  status              Show store state and secret counts
  list                List all secrets (name + category)
  set <name>          Add or update a secret (interactive or --stdin)
  delete <name>       Delete a secret
  info <name>         Show secret metadata and config references
  migrate             Auto-migrate plaintext keys from config.toml
  encrypt             Enable encryption (generate master key, encrypt all secrets)
  unlock              Unlock encrypted store (interactive or --stdin)
  lock                Lock encrypted store (clear key from OS credential store)
  rotate              Rotate master key (encrypted mode only)
  export              Export backup
  import              Import from backup
```

All subcommands communicate with the running Spacebot instance via the control API (`localhost:19898`). They don't access the secret store directly — this ensures the same locking/unlocking semantics apply regardless of whether the user uses the dashboard or CLI.
