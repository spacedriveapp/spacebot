//! OS-level filesystem containment for shell tool subprocesses.
//!
//! Replaces string-based command filtering with kernel-enforced boundaries.
//! On Linux, uses bubblewrap (bwrap) for mount namespace isolation.
//! On macOS, uses sandbox-exec with a generated SBPL profile.
//! Falls back to no sandboxing when neither backend is available.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

pub mod detection;

pub use detection::{SandboxBackend, detect_backend};

/// Sandbox configuration from the agent config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    #[serde(default = "default_mode")]
    pub mode: SandboxMode,
    #[serde(default)]
    pub writable_paths: Vec<PathBuf>,
    /// Environment variable names to forward from the parent process into worker
    /// subprocesses. This is the escape hatch for self-hosted users who set env
    /// vars in Docker/systemd but don't configure a secret store. When the secret
    /// store is available, `passthrough_env` is redundant — everything should be
    /// in the store. The field is additive either way.
    #[serde(default)]
    pub passthrough_env: Vec<String>,
    /// Refuse to start when `mode` is enabled but no backend can enforce it.
    ///
    /// `mode` records an intent; it has never recorded an outcome. Without this
    /// flag a host with no bubblewrap silently downgrades to unconfined
    /// execution while the config still reads `mode = "enabled"`. Setting this
    /// says the operator would rather the instance not come up than come up
    /// pretending. Defaults to false so upgrading changes no existing
    /// deployment's behaviour.
    #[serde(default)]
    pub require_containment: bool,
    /// Project root paths auto-injected into the sandbox allowlist.
    /// Managed by `refresh_project_paths`, not user-configured.
    #[serde(skip)]
    pub project_paths: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::Enabled,
            writable_paths: Vec::new(),
            passthrough_env: Vec::new(),
            require_containment: false,
            project_paths: Vec::new(),
        }
    }
}

impl SandboxConfig {
    /// All writable paths: user-configured + auto-injected project paths.
    pub fn all_writable_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.writable_paths.iter().chain(self.project_paths.iter())
    }
}

fn default_mode() -> SandboxMode {
    SandboxMode::Enabled
}

/// Sandbox enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    /// OS-level containment (default).
    Enabled,
    /// No containment, full host access.
    Disabled,
}

/// Whether OS-level containment is actually in force, and if not, why not.
///
/// Three states rather than one boolean because "off" and "on but inert" are
/// different operational facts with different fixes: one is a config decision
/// somebody made, the other is a missing package nobody noticed. Collapsing
/// them into `mode_enabled()` is what let this instance run unconfined while
/// its config file said otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainmentStatus {
    /// Sandbox mode is off in config. Nothing is contained, and nothing claims
    /// to be — an honest state, not a fault.
    Disabled,
    /// Mode is on and a backend is present: subprocesses are kernel-confined.
    Active { backend: SandboxBackend },
    /// Mode is on but no backend was detected. The config asks for containment
    /// this host cannot provide; subprocesses run with full host access.
    RequestedButInert,
}

impl ContainmentStatus {
    /// True only when a backend is actually confining subprocesses.
    pub fn is_active(self) -> bool {
        matches!(self, ContainmentStatus::Active { .. })
    }

    /// True when the config claims containment the host is not delivering.
    ///
    /// Callers that execute stored or unattended code should refuse on this,
    /// rather than on `!is_active()` — a deliberately disabled sandbox is a
    /// choice, an inert one is a surprise.
    pub fn is_inert(self) -> bool {
        matches!(self, ContainmentStatus::RequestedButInert)
    }

    /// The backend doing the confining, or `None` when nothing is.
    pub fn backend(self) -> Option<SandboxBackend> {
        match self {
            ContainmentStatus::Active { backend } => Some(backend),
            _ => None,
        }
    }
}

/// Raised when `require_containment` is set but no backend exists to honour it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainmentUnavailable;

impl std::fmt::Display for ContainmentUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sandbox.require_containment is set and sandbox.mode is \"enabled\", \
             but no sandbox backend was detected — {}",
            missing_backend_remediation()
        )
    }
}

impl std::error::Error for ContainmentUnavailable {}

/// What an operator has to do to turn an inert sandbox into a real one.
///
/// A warning that reports only "no backend available" leaves the reader to
/// find out on their own that the missing piece is a one-command install.
pub fn missing_backend_remediation() -> &'static str {
    if cfg!(target_os = "linux") {
        "install the `bubblewrap` package (e.g. `apt install bubblewrap`) and restart the instance"
    } else if cfg!(target_os = "macos") {
        "/usr/bin/sandbox-exec is missing, so this host cannot provide containment"
    } else {
        "no sandbox backend exists for this platform"
    }
}

/// Detected sandbox backend (internal version with proc_supported tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalBackend {
    /// Linux: bubblewrap available.
    Bubblewrap { proc_supported: bool },
    /// macOS: /usr/bin/sandbox-exec available.
    SandboxExec,
    /// No sandbox support detected, or mode = Disabled.
    None,
}

impl InternalBackend {
    /// Public view of the detected backend, dropping preflight detail.
    fn public(self) -> SandboxBackend {
        match self {
            InternalBackend::Bubblewrap { .. } => SandboxBackend::Bubblewrap,
            InternalBackend::SandboxExec => SandboxBackend::SandboxExec,
            InternalBackend::None => SandboxBackend::None,
        }
    }
}

/// Environment variables always passed through to worker subprocesses.
/// These are required for basic process operation.
const SAFE_ENV_VARS: &[&str] = &["USER", "LANG", "TERM"];

/// Environment variable names that are set by the hardened sandbox defaults and
/// must not be overridden via `passthrough_env`. Allowing user config to replace
/// PATH would drop `tools/bin` precedence; replacing HOME/TMPDIR would break the
/// deterministic sandbox-local paths. CI and DEBIAN_FRONTEND suppress interactive
/// prompts from npm, apt-get, and similar tools that would hang under stdin-less
/// execution.
const RESERVED_ENV_VARS: &[&str] = &["PATH", "HOME", "TMPDIR", "CI", "DEBIAN_FRONTEND"];

/// Env vars that enable library injection or alter runtime loading behavior.
/// Defense-in-depth: even if the tool-level blocklist is bypassed, the sandbox
/// layer will silently drop these from per-command env vars.
const DANGEROUS_ENV_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "NODE_OPTIONS",
    "RUBYOPT",
    "PERL5OPT",
    "PERL5LIB",
    "BASH_ENV",
    "ENV",
];

/// Returns true if the variable name is reserved (set by hardened defaults) or
/// is in the safe-vars list, and therefore must not be overridden by
/// `passthrough_env` or per-command env vars.
fn is_reserved_env_var(name: &str) -> bool {
    RESERVED_ENV_VARS.contains(&name) || SAFE_ENV_VARS.contains(&name)
}

/// Returns true if the variable name enables library injection or alters
/// runtime loading behavior.
fn is_dangerous_env_var(name: &str) -> bool {
    DANGEROUS_ENV_VARS
        .iter()
        .any(|blocked| name.eq_ignore_ascii_case(blocked))
}

/// Linux host paths exposed read-only inside bubblewrap sandboxes.
/// This is a minimal runtime allowlist: worker/user data directories are not
/// mounted unless they are explicitly configured as writable paths.
const LINUX_READ_ONLY_SYSTEM_PATHS: &[&str] = &[
    "/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc", "/opt", "/run", "/nix",
];

/// macOS host paths exposed read-only in sandbox-exec profiles.
/// User data directories are intentionally excluded; worker access is limited
/// to workspace paths plus core system roots.
const MACOS_READ_ONLY_SYSTEM_PATHS: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/opt",
    "/Library",
    "/Applications",
    "/private/etc",
    "/private/var/run",
    "/private/tmp",
    "/etc",
    "/dev",
];

/// Filesystem sandbox for subprocess execution.
///
/// Created once per agent at startup, shared via `Arc` across all workers.
/// Wraps `tokio::process::Command` to apply OS-level containment before spawning.
///
/// Reads `SandboxMode` dynamically from the shared `ArcSwap<SandboxConfig>` on
/// every `wrap()` call, so toggling sandbox mode via the API takes effect
/// immediately without restarting the agent.
pub struct Sandbox {
    config: Arc<ArcSwap<SandboxConfig>>,
    workspace: PathBuf,
    data_dir: PathBuf,
    tools_bin: PathBuf,
    backend: InternalBackend,
    /// Owning agent for this sandbox. Used to scope tool-secret reads when
    /// the secrets store is wired in (see `tool_secrets`). Sandbox is per-
    /// agent in production; the test constructor uses a placeholder ID.
    agent_id: crate::AgentId,
    /// Reference to the secrets store for injecting tool secrets into worker
    /// subprocesses. When set, `wrap()` reads tool secrets from the store and
    /// injects them as env vars via `--setenv` (bubblewrap) or `Command::env()`
    /// (passthrough/sandbox-exec).
    secrets_store: ArcSwap<Option<Arc<crate::secrets::store::SecretsStore>>>,
}

impl std::fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let config = self.config.load();
        f.debug_struct("Sandbox")
            .field("mode", &config.mode)
            .field("workspace", &self.workspace)
            .field("data_dir", &self.data_dir)
            .field("tools_bin", &self.tools_bin)
            .field("backend", &self.backend)
            .finish()
    }
}

impl Sandbox {
    /// Create a sandbox with the given configuration. Probes for backend support.
    ///
    /// Always detects the best available backend regardless of the initial mode,
    /// so switching from Disabled to Enabled via the API works without restart.
    pub async fn new(
        config: Arc<ArcSwap<SandboxConfig>>,
        workspace: PathBuf,
        instance_dir: &Path,
        data_dir: PathBuf,
        agent_id: crate::AgentId,
    ) -> Self {
        let tools_bin = instance_dir.join("tools/bin");

        // Always detect the backend so we know what's available if the user
        // later enables sandboxing via the API.
        let backend = detect_backend_internal().await;
        let current_mode = config.load().mode;

        match backend {
            InternalBackend::Bubblewrap { proc_supported } => {
                if current_mode == SandboxMode::Enabled {
                    tracing::info!(proc_supported, "sandbox enabled: bubblewrap backend");
                } else {
                    tracing::info!(
                        proc_supported,
                        "sandbox disabled by config (bubblewrap available)"
                    );
                }
            }
            InternalBackend::SandboxExec => {
                if current_mode == SandboxMode::Enabled {
                    tracing::info!("sandbox enabled: macOS sandbox-exec backend");
                } else {
                    tracing::info!("sandbox disabled by config (sandbox-exec available)");
                }
            }
            InternalBackend::None if current_mode == SandboxMode::Enabled => {
                // The loudest this fact ever gets to be. Nothing downstream
                // surfaces it: the allowlists come back empty, `wrap` returns a
                // plain Command, and every log line after this looks normal.
                tracing::warn!(
                    agent = %agent_id,
                    remediation = missing_backend_remediation(),
                    "SANDBOX INERT: sandbox.mode is \"enabled\" but no sandbox backend was \
                     detected. Shell commands run with NO OS-level containment — only the \
                     file tools' voluntary path checks remain, and a shell command does not \
                     go through them. Set sandbox.require_containment = true to refuse \
                     startup instead of running unconfined."
                );
            }
            InternalBackend::None => {
                tracing::info!("sandbox disabled by config (no backend available)");
            }
        }

        // Canonicalize paths at construction to resolve symlinks and validate existence.
        let workspace = canonicalize_or_self(&workspace);
        let data_dir = canonicalize_or_self(&data_dir);

        Self {
            config,
            workspace,
            data_dir,
            tools_bin,
            backend,
            agent_id,
            secrets_store: ArcSwap::from_pointee(None),
        }
    }

    /// Set the secrets store for tool secret injection into worker subprocesses.
    ///
    /// Called after the secrets store is initialized (may happen after sandbox
    /// construction during agent startup).
    pub fn set_secrets_store(&self, store: Arc<crate::secrets::store::SecretsStore>) {
        self.secrets_store.store(Arc::new(Some(store)));
    }

    /// Read tool secrets visible to this sandbox's owning agent for injection
    /// into subprocess environment.
    fn tool_secrets(&self) -> HashMap<String, String> {
        let guard = self.secrets_store.load();
        match guard.as_ref() {
            Some(store) => store.tool_env_vars(&self.agent_id),
            None => HashMap::new(),
        }
    }

    /// True when sandbox mode is enabled in config.
    ///
    /// This is a statement about configuration, not about enforcement. It is
    /// true on a host with no backend, where nothing is enforced at all — use
    /// `containment_status` for anything that depends on the outcome.
    pub fn mode_enabled(&self) -> bool {
        self.config.load().mode == SandboxMode::Enabled
    }

    /// The configured mode, as written in config.
    pub fn mode(&self) -> SandboxMode {
        self.config.load().mode
    }

    /// Whether this agent is configured to refuse running without containment.
    pub fn require_containment(&self) -> bool {
        self.config.load().require_containment
    }

    /// The backend detected on this host, independent of whether mode is on.
    ///
    /// Detection runs regardless of mode so that enabling the sandbox via the
    /// API does not require a restart; this reports what that probe found.
    pub fn detected_backend(&self) -> SandboxBackend {
        self.backend.public()
    }

    /// Get the workspace directory path.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Update the sandbox allowlist with project root paths.
    ///
    /// Merges the given project root paths into the sandbox config alongside
    /// the user-configured `writable_paths`. Takes effect immediately — the
    /// next `wrap()` call will include these paths.
    pub fn refresh_project_paths(&self, paths: Vec<PathBuf>) {
        self.config.rcu(|current| {
            let mut next = (**current).clone();
            next.project_paths = paths.clone();
            Arc::new(next)
        });
    }

    /// Check whether a canonical path falls within the workspace or any
    /// allowed writable path (user-configured or project-injected).
    ///
    /// Used by shell/file tools to relax the workspace boundary when
    /// project paths are registered.
    pub fn is_path_allowed(&self, canonical: &Path) -> bool {
        let workspace_canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        if canonical.starts_with(&workspace_canonical) {
            return true;
        }
        let config = self.config.load();
        for path in config.all_writable_paths() {
            let allowed = path.canonicalize().unwrap_or_else(|_| path.clone());
            if canonical.starts_with(&allowed) {
                return true;
            }
        }
        false
    }

    /// What containment this sandbox is actually providing right now.
    ///
    /// The one place that joins the configured intent to the detected backend.
    /// Anything that needs to *act* on containment — refuse to run stored
    /// commands, report status to an operator — should ask this rather than
    /// reconstruct the join from `mode_enabled` and a backend check, which is
    /// how the two drifted apart in the first place.
    pub fn containment_status(&self) -> ContainmentStatus {
        if !self.mode_enabled() {
            return ContainmentStatus::Disabled;
        }
        match self.backend.public() {
            SandboxBackend::None => ContainmentStatus::RequestedButInert,
            backend => ContainmentStatus::Active { backend },
        }
    }

    /// True when OS-level containment is currently active.
    ///
    /// If mode is enabled but no backend is available, this returns false
    /// because subprocesses fall back to passthrough execution.
    pub fn containment_active(&self) -> bool {
        self.containment_status().is_active()
    }

    /// Fail closed when the config demands containment this host cannot give.
    ///
    /// Only the inert case is an error. A sandbox that is off by choice is not
    /// a broken promise, and one that is genuinely active is the requirement
    /// being met — refusing on either would make `require_containment` a flag
    /// nobody could safely leave on.
    pub fn verify_required_containment(&self) -> Result<(), ContainmentUnavailable> {
        if self.config.load().require_containment && self.containment_status().is_inert() {
            return Err(ContainmentUnavailable);
        }
        Ok(())
    }

    /// Read-allowlisted filesystem paths exposed to shell subprocesses when
    /// containment is active.
    pub fn prompt_read_allowlist(&self) -> Vec<String> {
        if !self.containment_active() {
            return Vec::new();
        }

        let config = self.config.load();
        let mut paths = Vec::new();

        match self.backend {
            InternalBackend::Bubblewrap { .. } => {
                for system_path in LINUX_READ_ONLY_SYSTEM_PATHS {
                    let path = Path::new(system_path);
                    if path.exists() {
                        push_unique_path(&mut paths, canonicalize_or_self(path));
                    }
                }

                if self.tools_bin.exists() {
                    push_unique_path(&mut paths, canonicalize_or_self(&self.tools_bin));
                }

                push_unique_path(&mut paths, canonicalize_or_self(&self.workspace));

                for path in config.all_writable_paths() {
                    if let Ok(canonical) = path.canonicalize() {
                        push_unique_path(&mut paths, canonical);
                    }
                }
            }
            InternalBackend::SandboxExec => {
                for system_path in MACOS_READ_ONLY_SYSTEM_PATHS {
                    let path = Path::new(system_path);
                    if path.exists() {
                        push_unique_path(&mut paths, canonicalize_or_self(path));
                    }
                }

                if self.tools_bin.exists() {
                    push_unique_path(&mut paths, canonicalize_or_self(&self.tools_bin));
                }

                push_unique_path(&mut paths, canonicalize_or_self(&self.workspace));

                for path in config.all_writable_paths() {
                    push_unique_path(&mut paths, canonicalize_or_self(path));
                }
            }
            InternalBackend::None => {}
        }

        paths
    }

    /// Write-allowlisted filesystem paths exposed to shell subprocesses when
    /// containment is active.
    pub fn prompt_write_allowlist(&self) -> Vec<String> {
        if !self.containment_active() {
            return Vec::new();
        }

        let config = self.config.load();
        let mut paths = Vec::new();

        push_unique_path(&mut paths, canonicalize_or_self(&self.workspace));
        push_unique_path(&mut paths, canonicalize_or_self(Path::new("/tmp")));

        match self.backend {
            InternalBackend::Bubblewrap { .. } => {
                for path in config.all_writable_paths() {
                    if let Ok(canonical) = path.canonicalize() {
                        push_unique_path(&mut paths, canonical);
                    }
                }
            }
            InternalBackend::SandboxExec => {
                for path in config.all_writable_paths() {
                    push_unique_path(&mut paths, canonicalize_or_self(path));
                }
            }
            InternalBackend::None => {}
        }

        paths
    }

    /// Wrap a command for sandboxed execution.
    ///
    /// Returns a `Command` ready to spawn, potentially prefixed with bwrap or
    /// sandbox-exec depending on the detected backend. The caller still needs
    /// to set stdout/stderr/timeout after this call.
    ///
    /// `command_env` contains per-command environment variables set by the tool
    /// caller (e.g. shell tool `env` parameter). These are injected via
    /// `--setenv` for bubblewrap or `.env()` for sandbox-exec/passthrough, so
    /// they correctly reach the inner sandboxed process regardless of backend.
    ///
    /// Reads the current `SandboxMode` from the shared `ArcSwap<SandboxConfig>`
    /// on every call, so changes via the API take effect immediately.
    pub fn wrap(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        command_env: &HashMap<String, String>,
    ) -> Command {
        self.wrap_inner(program, args, working_dir, command_env, true)
    }

    /// [`Sandbox::wrap`], with the agent's tool secrets left out of the child's
    /// environment.
    ///
    /// For stored, unattended execution — a workflow command step. A worker gets
    /// the secrets because a worker is trusted to use them, in the moment, for
    /// work someone asked for. A template command is authored once and runs
    /// forever, so the credentials it can reach should be the ones somebody
    /// deliberately gave it rather than every credential the agent holds.
    pub fn wrap_without_tool_secrets(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        command_env: &HashMap<String, String>,
    ) -> Command {
        self.wrap_inner(program, args, working_dir, command_env, false)
    }

    fn wrap_inner(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        command_env: &HashMap<String, String>,
        inject_tool_secrets: bool,
    ) -> Command {
        let config = self.config.load();

        // Prepend tools/bin to PATH for all commands
        let path_env = match std::env::var_os("PATH") {
            Some(current) => {
                let mut paths = std::env::split_paths(&current).collect::<Vec<_>>();
                paths.insert(0, self.tools_bin.clone());
                std::env::join_paths(paths)
                    .unwrap_or(current)
                    .to_string_lossy()
                    .into_owned()
            }
            None => self.tools_bin.to_string_lossy().into_owned(),
        };

        // Read tool secrets once for injection into the subprocess.
        let tool_secrets = if inject_tool_secrets {
            self.tool_secrets()
        } else {
            HashMap::new()
        };

        if config.mode == SandboxMode::Disabled {
            return self.wrap_passthrough(
                program,
                args,
                working_dir,
                &path_env,
                &config,
                &tool_secrets,
                command_env,
            );
        }

        match self.backend {
            InternalBackend::Bubblewrap { proc_supported } => self.wrap_bubblewrap(
                program,
                args,
                working_dir,
                proc_supported,
                &path_env,
                &config,
                &tool_secrets,
                command_env,
            ),
            InternalBackend::SandboxExec => self.wrap_sandbox_exec(
                program,
                args,
                working_dir,
                &path_env,
                &config,
                &tool_secrets,
                command_env,
            ),
            InternalBackend::None => self.wrap_passthrough(
                program,
                args,
                working_dir,
                &path_env,
                &config,
                &tool_secrets,
                command_env,
            ),
        }
    }

    /// Linux: wrap with bubblewrap mount namespace.
    #[allow(clippy::too_many_arguments)]
    fn wrap_bubblewrap(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        proc_supported: bool,
        path_env: &str,
        config: &SandboxConfig,
        tool_secrets: &HashMap<String, String>,
        command_env: &HashMap<String, String>,
    ) -> Command {
        let mut cmd = Command::new("bwrap");

        // Mount order matters — later mounts override earlier ones.
        // 1. Mount a minimal read-only runtime allowlist.
        for system_path in LINUX_READ_ONLY_SYSTEM_PATHS {
            let path = Path::new(system_path);
            if path.exists() {
                cmd.arg("--ro-bind").arg(path).arg(path);
            }
        }

        // Keep persistent tools visible on PATH if present.
        if self.tools_bin.exists() {
            cmd.arg("--ro-bind")
                .arg(&self.tools_bin)
                .arg(&self.tools_bin);
        }

        // 2. Writable /dev with standard nodes
        cmd.arg("--dev").arg("/dev");

        // 3. Fresh /proc (if supported by the environment)
        if proc_supported {
            cmd.arg("--proc").arg("/proc");
        }

        // 4. Private /tmp per invocation
        cmd.arg("--tmpfs").arg("/tmp");

        // 5. Workspace writable
        cmd.arg("--bind").arg(&self.workspace).arg(&self.workspace);

        // 6. Each configured + project writable path (canonicalized dynamically)
        for path in config.all_writable_paths() {
            match path.canonicalize() {
                Ok(canonical) => {
                    cmd.arg("--bind").arg(&canonical).arg(&canonical);
                }
                Err(error) => {
                    tracing::debug!(
                        path = %path.display(),
                        %error,
                        "skipping writable_path (does not exist or is unresolvable)"
                    );
                }
            }
        }

        // 7. Mask agent data dir with an empty tmpfs to prevent reads/writes,
        // even when it overlaps with workspace-related paths.
        cmd.arg("--tmpfs").arg(&self.data_dir);

        // 8. Isolation flags
        cmd.arg("--unshare-pid");
        cmd.arg("--new-session");
        cmd.arg("--die-with-parent");

        // 9. Clear all inherited environment variables. Workers must not see
        // system secrets (LLM API keys, messaging tokens) or SPACEBOT_* internals.
        cmd.arg("--clearenv");

        // 10. Working directory
        cmd.arg("--chdir").arg(working_dir);

        // 11. Set PATH inside the sandbox
        cmd.arg("--setenv").arg("PATH").arg(path_env);

        // 12. Set deterministic sandbox-local home/temp paths.
        cmd.arg("--setenv")
            .arg("HOME")
            .arg(self.workspace.to_string_lossy().into_owned());
        cmd.arg("--setenv").arg("TMPDIR").arg("/tmp");

        // 12a. Suppress interactive prompts. CI=true prevents npm/npx/yarn
        // from prompting; DEBIAN_FRONTEND=noninteractive prevents apt-get.
        // Shell tool runs without stdin, so interactive prompts always hang.
        cmd.arg("--setenv").arg("CI").arg("true");
        cmd.arg("--setenv")
            .arg("DEBIAN_FRONTEND")
            .arg("noninteractive");

        // 13. Re-inject safe environment variables for basic process operation
        for var_name in SAFE_ENV_VARS {
            if let Ok(value) = std::env::var(var_name) {
                cmd.arg("--setenv").arg(var_name).arg(value);
            }
        }

        // 13. Re-inject tool secrets from the secret store.
        // Only tool-category secrets are injected; system secrets (LLM API keys,
        // messaging tokens) never enter subprocess environments.
        for (name, value) in tool_secrets {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved tool secret name");
                continue;
            }
            cmd.arg("--setenv").arg(name).arg(value);
        }

        // 14. Re-inject passthrough env vars (user-configured forwarding),
        // skipping any that would override hardened defaults.
        for var_name in &config.passthrough_env {
            if is_reserved_env_var(var_name) {
                tracing::debug!(%var_name, "skipping reserved passthrough_env variable");
                continue;
            }
            if let Ok(value) = std::env::var(var_name) {
                cmd.arg("--setenv").arg(var_name).arg(value);
            }
        }

        // 15. Per-command env vars from tool caller (e.g. shell tool `env`).
        // Injected via --setenv so they reach the inner sandboxed process.
        for (name, value) in command_env {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved per-command env var");
                continue;
            }
            if is_dangerous_env_var(name) {
                tracing::warn!(%name, "dropping dangerous per-command env var");
                continue;
            }
            cmd.arg("--setenv").arg(name).arg(value);
        }

        // 16. Worker keyring isolation (Linux) — give the child a fresh empty
        // session keyring so it cannot access the parent's keyring (which holds
        // the master key for secret store encryption).
        #[cfg(target_os = "linux")]
        {
            // pre_exec runs between fork and exec. If it fails, spawn() fails
            // and the worker is not started (correct — a worker that inherits
            // the parent's session keyring could access the master key).
            unsafe {
                cmd.pre_exec(|| crate::secrets::keystore::pre_exec_new_session_keyring());
            }
        }

        // 17. The actual command
        cmd.arg("--").arg(program);
        for arg in args {
            cmd.arg(arg);
        }

        cmd
    }

    /// macOS: wrap with sandbox-exec and a generated SBPL profile.
    #[allow(clippy::too_many_arguments)]
    fn wrap_sandbox_exec(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        path_env: &str,
        config: &SandboxConfig,
        tool_secrets: &HashMap<String, String>,
        command_env: &HashMap<String, String>,
    ) -> Command {
        let profile = self.generate_sbpl_profile(config);

        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-p").arg(profile);
        cmd.arg(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.current_dir(working_dir);

        // Clear all inherited environment variables, then re-inject only
        // approved vars. Prevents system secrets from leaking to workers.
        cmd.env_clear();
        cmd.env("PATH", path_env);
        cmd.env("HOME", &self.workspace);
        cmd.env("TMPDIR", "/tmp");
        cmd.env("CI", "true");
        cmd.env("DEBIAN_FRONTEND", "noninteractive");
        for var_name in SAFE_ENV_VARS {
            if let Ok(value) = std::env::var(var_name) {
                cmd.env(var_name, value);
            }
        }
        // Inject tool secrets from the secret store.
        for (name, value) in tool_secrets {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved tool secret name");
                continue;
            }
            cmd.env(name, value);
        }
        for var_name in &config.passthrough_env {
            if is_reserved_env_var(var_name) {
                tracing::debug!(%var_name, "skipping reserved passthrough_env variable");
                continue;
            }
            if let Ok(value) = std::env::var(var_name) {
                cmd.env(var_name, value);
            }
        }
        // Per-command env vars from tool caller.
        for (name, value) in command_env {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved per-command env var");
                continue;
            }
            if is_dangerous_env_var(name) {
                tracing::warn!(%name, "dropping dangerous per-command env var");
                continue;
            }
            cmd.env(name, value);
        }

        cmd
    }

    /// No backend: pass through without OS-level containment.
    ///
    /// Still applies environment sanitization — workers never inherit the full
    /// parent environment regardless of sandbox state.
    #[allow(clippy::too_many_arguments)]
    fn wrap_passthrough(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        path_env: &str,
        config: &SandboxConfig,
        tool_secrets: &HashMap<String, String>,
        command_env: &HashMap<String, String>,
    ) -> Command {
        let mut cmd = Command::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.current_dir(working_dir);

        let home_dir = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.workspace.as_os_str().to_os_string());

        // Clear all inherited environment variables, then re-inject only
        // approved vars. Prevents system secrets from leaking to workers.
        cmd.env_clear();
        cmd.env("PATH", path_env);
        cmd.env("HOME", home_dir);
        cmd.env("TMPDIR", "/tmp");
        cmd.env("CI", "true");
        cmd.env("DEBIAN_FRONTEND", "noninteractive");
        for var_name in SAFE_ENV_VARS {
            if let Ok(value) = std::env::var(var_name) {
                cmd.env(var_name, value);
            }
        }
        // Inject tool secrets from the secret store.
        for (name, value) in tool_secrets {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved tool secret name");
                continue;
            }
            cmd.env(name, value);
        }
        for var_name in &config.passthrough_env {
            if is_reserved_env_var(var_name) {
                tracing::debug!(%var_name, "skipping reserved passthrough_env variable");
                continue;
            }
            if let Ok(value) = std::env::var(var_name) {
                cmd.env(var_name, value);
            }
        }
        // Per-command env vars from tool caller.
        for (name, value) in command_env {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved per-command env var");
                continue;
            }
            if is_dangerous_env_var(name) {
                tracing::warn!(%name, "dropping dangerous per-command env var");
                continue;
            }
            cmd.env(name, value);
        }

        // Mirror the sandboxed path's process isolation (bwrap's
        // --new-session): the child leads its own process group, so the
        // worker's whole tree stays addressable as a single unit and
        // signals aimed at spacebot's group (e.g. terminal SIGINT) don't
        // reach workers that outlive the request that spawned them.
        #[cfg(unix)]
        cmd.process_group(0);

        // Worker keyring isolation (Linux) — give the child a fresh empty
        // session keyring even in passthrough (no sandbox) mode.
        #[cfg(target_os = "linux")]
        {
            unsafe {
                cmd.pre_exec(|| crate::secrets::keystore::pre_exec_new_session_keyring());
            }
        }

        cmd
    }

    /// Generate a macOS SBPL (Sandbox Profile Language) policy.
    ///
    /// Paths are canonicalized because /var on macOS is actually /private/var.
    fn generate_sbpl_profile(&self, config: &SandboxConfig) -> String {
        let workspace = canonicalize_or_self(&self.workspace);
        let tools_bin = canonicalize_or_self(&self.tools_bin);

        let mut profile = String::from(
            r#"(version 1)
(deny default)

; process basics
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
"#,
        );

        profile.push_str("\n; filesystem: read allowlist (system roots + workspace)\n");
        // Allow access to the root directory entry itself so macOS can traverse
        // into explicitly-allowed subpaths without granting recursive read access.
        profile.push_str("(allow file-read* (literal \"/\"))\n");
        for system_path in MACOS_READ_ONLY_SYSTEM_PATHS {
            let path = Path::new(system_path);
            if path.exists() {
                let canonical = canonicalize_or_self(path);
                profile.push_str(&format!(
                    "(allow file-read* (subpath \"{}\"))\n",
                    escape_sbpl_path(&canonical)
                ));
            }
        }

        profile.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl_path(&workspace)
        ));

        if self.tools_bin.exists() {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                escape_sbpl_path(&tools_bin)
            ));
        }

        profile.push('\n');

        // Workspace writable
        profile.push_str(&format!(
            "; workspace writable\n(allow file-write* (subpath \"{}\"))\n\n",
            escape_sbpl_path(&workspace)
        ));

        // Additional writable paths (user-configured + project paths) are readable and writable.
        for (index, path) in config.all_writable_paths().enumerate() {
            let canonical = canonicalize_or_self(path);
            profile.push_str(&format!(
                "; writable path {index}\n(allow file-read* (subpath \"{}\"))\n(allow file-write* (subpath \"{}\"))\n",
                escape_sbpl_path(&canonical),
                escape_sbpl_path(&canonical)
            ));
        }

        // /tmp writable
        let tmp = canonicalize_or_self(Path::new("/tmp"));
        profile.push_str(&format!(
            "\n; tmp writable\n(allow file-write* (subpath \"{}\"))\n",
            escape_sbpl_path(&tmp)
        ));

        // Protect data_dir even if it falls under the workspace subtree
        let data_dir = canonicalize_or_self(&self.data_dir);
        profile.push_str(&format!(
            "\n; data dir blocked\n(deny file-read* (subpath \"{}\"))\n(deny file-write* (subpath \"{}\"))\n",
            escape_sbpl_path(&data_dir),
            escape_sbpl_path(&data_dir)
        ));

        profile.push_str(
            r#"
; dev, sysctl, mach for basic operation
(allow file-write-data
  (require-all (path "/dev/null") (vnode-type CHARACTER-DEVICE)))
(allow sysctl-read)
(allow mach-lookup
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.trustd"))
(allow ipc-posix-sem)
(allow pseudo-tty)
(allow network*)
"#,
        );

        profile
    }

    /// Create a minimal sandbox for unit tests without probing for backends.
    ///
    /// Backend is `None`, which is also the state of any host without
    /// bubblewrap — so this constructor reproduces the inert case by default.
    #[cfg(test)]
    pub fn new_for_test(config: Arc<ArcSwap<SandboxConfig>>, workspace: PathBuf) -> Self {
        Self {
            config,
            workspace,
            data_dir: PathBuf::new(),
            tools_bin: PathBuf::new(),
            backend: InternalBackend::None,
            agent_id: std::sync::Arc::from("test-agent"),
            secrets_store: ArcSwap::from_pointee(None),
        }
    }

    /// Test sandbox pinned to a specific backend, so the active-containment
    /// cases can be asserted on a host that has no backend installed.
    #[cfg(test)]
    pub fn new_for_test_with_backend(
        config: Arc<ArcSwap<SandboxConfig>>,
        workspace: PathBuf,
        backend: SandboxBackend,
    ) -> Self {
        let backend = match backend {
            SandboxBackend::Bubblewrap => InternalBackend::Bubblewrap {
                proc_supported: true,
            },
            SandboxBackend::SandboxExec => InternalBackend::SandboxExec,
            SandboxBackend::None => InternalBackend::None,
        };
        Self {
            backend,
            ..Self::new_for_test(config, workspace)
        }
    }
}

/// Push a path into a list while preserving order and removing duplicates.
fn push_unique_path(paths: &mut Vec<String>, path: PathBuf) {
    let value = path.display().to_string();
    if !paths.contains(&value) {
        paths.push(value);
    }
}

/// Escape a path for embedding in an SBPL string literal.
fn escape_sbpl_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Canonicalize a path, falling back to the original if canonicalization fails.
fn canonicalize_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Detect the best available sandbox backend for the current platform.
async fn detect_backend_internal() -> InternalBackend {
    if cfg!(target_os = "linux") {
        detect_bubblewrap().await
    } else if cfg!(target_os = "macos") {
        detect_sandbox_exec()
    } else {
        tracing::warn!("no sandbox backend available for this platform");
        InternalBackend::None
    }
}

fn bubblewrap_true_binary() -> &'static str {
    if Path::new("/bin/true").exists() {
        "/bin/true"
    } else {
        "/usr/bin/true"
    }
}

/// Linux: check if bwrap is available and whether --proc /proc works.
async fn detect_bubblewrap() -> InternalBackend {
    // Check if bwrap exists
    let version_check = Command::new("bwrap").arg("--version").output().await;

    match version_check {
        Ok(output) if output.status.success() => {}
        Ok(_) => {
            tracing::debug!("bwrap not found in PATH");
            return InternalBackend::None;
        }
        Err(_) => {
            tracing::debug!("bwrap not found in PATH");
            return InternalBackend::None;
        }
    }

    // Preflight: test if --proc /proc works (may fail in nested containers)
    let proc_check = Command::new("bwrap")
        .args([
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--",
            bubblewrap_true_binary(),
        ])
        .output()
        .await;

    let proc_supported = proc_check.is_ok_and(|output| output.status.success());

    if !proc_supported {
        tracing::debug!("bwrap --proc /proc not supported, running without fresh procfs");
    }

    InternalBackend::Bubblewrap { proc_supported }
}

/// macOS: check if sandbox-exec exists at its known path.
fn detect_sandbox_exec() -> InternalBackend {
    if Path::new("/usr/bin/sandbox-exec").exists() {
        InternalBackend::SandboxExec
    } else {
        tracing::debug!("/usr/bin/sandbox-exec not found");
        InternalBackend::None
    }
}

/// Signal the process group led by `pid` (Unix only).
///
/// tokio's `kill`/`kill_on_drop` only signal the direct child, so a worker
/// that forked grandchildren — a shell that launched a server, a build that
/// spawned compilers — leaks them past a timeout. Children spawned through
/// `Sandbox::wrap` lead their own group (`process_group(0)` in passthrough,
/// `--new-session` under bwrap), which makes the whole tree addressable as
/// one unit. A missing group (already exited, or a backend whose outermost
/// process leads no group) is not an error: the caller's direct kill remains
/// the fallback.
#[cfg(unix)]
pub fn kill_process_group(pid: u32) {
    // A negative pid signals the whole group. ESRCH — the group is already
    // gone — is the expected failure and needs no escalation.
    if unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) } != 0 {
        tracing::debug!(pid, "process group already gone or pid leads no group");
    }
}

/// No process groups on this platform; the direct kill is all there is.
#[cfg(not(unix))]
pub fn kill_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a sandbox over the given mode / requirement, with no backend —
    /// the state of any host that has not installed bubblewrap.
    fn sandbox_without_backend(mode: SandboxMode, require_containment: bool) -> Sandbox {
        Sandbox::new_for_test(
            Arc::new(ArcSwap::from_pointee(SandboxConfig {
                mode,
                require_containment,
                ..SandboxConfig::default()
            })),
            PathBuf::from("/tmp"),
        )
    }

    /// Same, but with a backend present, so the genuinely-contained cases can
    /// be asserted regardless of what the test host has installed.
    fn sandbox_with_backend(mode: SandboxMode, require_containment: bool) -> Sandbox {
        Sandbox::new_for_test_with_backend(
            Arc::new(ArcSwap::from_pointee(SandboxConfig {
                mode,
                require_containment,
                ..SandboxConfig::default()
            })),
            PathBuf::from("/tmp"),
            SandboxBackend::Bubblewrap,
        )
    }

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.mode, SandboxMode::Enabled);
        assert!(config.writable_paths.is_empty());
        assert!(config.project_paths.is_empty());
        assert!(config.passthrough_env.is_empty());
    }

    /// `require_containment` must default off, or upgrading Spacebot on a host
    /// with no bubblewrap turns a running instance into one that will not boot.
    #[test]
    fn require_containment_defaults_off_so_upgrades_do_not_change_behaviour() {
        assert!(!SandboxConfig::default().require_containment);

        let parsed: SandboxConfig =
            toml::from_str("mode = \"enabled\"").expect("parse a config that omits the field");
        assert!(!parsed.require_containment);
    }

    /// The bug this whole change exists for: `mode = "enabled"` with no backend
    /// installed is not containment. If this ever returns true, every caller
    /// that gates dangerous work on `containment_active` silently opens up.
    #[test]
    fn containment_is_not_active_when_mode_is_enabled_but_no_backend_exists() {
        let sandbox = sandbox_without_backend(SandboxMode::Enabled, false);

        assert!(sandbox.mode_enabled(), "config asked for a sandbox");
        assert_eq!(sandbox.detected_backend(), SandboxBackend::None);
        assert!(
            !sandbox.containment_active(),
            "no backend means nothing is confined, whatever the config says"
        );
    }

    /// The three states must stay distinguishable. Collapsing "off by choice"
    /// into "on but inert" would either hide a broken deployment or cry wolf
    /// on a deliberately unsandboxed one.
    #[test]
    fn containment_status_distinguishes_disabled_active_and_inert() {
        assert_eq!(
            sandbox_without_backend(SandboxMode::Disabled, false).containment_status(),
            ContainmentStatus::Disabled
        );
        assert_eq!(
            sandbox_with_backend(SandboxMode::Disabled, false).containment_status(),
            ContainmentStatus::Disabled,
            "a present backend does not contain anything while mode is off"
        );
        assert_eq!(
            sandbox_with_backend(SandboxMode::Enabled, false).containment_status(),
            ContainmentStatus::Active {
                backend: SandboxBackend::Bubblewrap
            }
        );
        assert_eq!(
            sandbox_without_backend(SandboxMode::Enabled, false).containment_status(),
            ContainmentStatus::RequestedButInert
        );
    }

    /// `is_inert` is what unattended executors gate on. It must be true only
    /// for the requested-but-unenforced case — a disabled sandbox is a choice,
    /// not a broken promise, and an active one is fine.
    #[test]
    fn only_the_inert_status_reports_a_broken_containment_promise() {
        assert!(!ContainmentStatus::Disabled.is_inert());
        assert!(
            !ContainmentStatus::Active {
                backend: SandboxBackend::Bubblewrap
            }
            .is_inert()
        );
        assert!(ContainmentStatus::RequestedButInert.is_inert());

        assert!(!ContainmentStatus::Disabled.is_active());
        assert!(
            ContainmentStatus::Active {
                backend: SandboxBackend::SandboxExec
            }
            .is_active()
        );
        assert!(!ContainmentStatus::RequestedButInert.is_active());
    }

    /// `require_containment` must refuse in exactly one case. Refusing when the
    /// sandbox is off would make the flag unusable for anyone who disables the
    /// sandbox deliberately; refusing when containment is real would take down
    /// correctly configured hosts.
    #[test]
    fn require_containment_refuses_only_when_containment_is_requested_but_inert() {
        assert!(
            sandbox_without_backend(SandboxMode::Enabled, true)
                .verify_required_containment()
                .is_err(),
            "config demanded containment the host cannot provide"
        );

        assert!(
            sandbox_with_backend(SandboxMode::Enabled, true)
                .verify_required_containment()
                .is_ok(),
            "containment is genuinely active; the requirement is met"
        );
        assert!(
            sandbox_without_backend(SandboxMode::Disabled, true)
                .verify_required_containment()
                .is_ok(),
            "mode is off, so nothing was promised to break"
        );
        assert!(
            sandbox_without_backend(SandboxMode::Enabled, false)
                .verify_required_containment()
                .is_ok(),
            "default-off must not change existing deployments"
        );
    }

    /// Pins the current honest behaviour of the allowlists. `wrap` builds a
    /// plain Command when containment is inert, so an allowlist that started
    /// coming back populated would advertise a boundary nothing enforces.
    #[test]
    fn prompt_allowlists_stay_empty_when_containment_is_inert() {
        let sandbox = sandbox_without_backend(SandboxMode::Enabled, false);

        assert!(sandbox.containment_status().is_inert());
        assert!(
            sandbox.prompt_read_allowlist().is_empty(),
            "an inert sandbox must not advertise read boundaries it cannot enforce"
        );
        assert!(
            sandbox.prompt_write_allowlist().is_empty(),
            "an inert sandbox must not advertise write boundaries it cannot enforce"
        );
    }

    /// The warning is the highest-value part of this change; if it stops naming
    /// the package, the operator is left to work out the fix themselves.
    #[test]
    fn missing_backend_remediation_names_the_thing_to_install() {
        let remediation = missing_backend_remediation();
        if cfg!(target_os = "linux") {
            assert!(remediation.contains("bubblewrap"), "got: {remediation}");
        }
        assert!(
            ContainmentUnavailable.to_string().contains(remediation),
            "the startup refusal must carry the fix, not just the fault"
        );
    }

    /// A passthrough child leads its own process group, mirroring the
    /// sandboxed path's `--new-session`: the worker tree stays addressable as
    /// one unit rather than dissolving into spacebot's group.
    #[cfg(unix)]
    #[tokio::test]
    async fn passthrough_child_leads_its_own_process_group() {
        let sandbox = sandbox_without_backend(SandboxMode::Enabled, false);
        let mut command = sandbox.wrap("sleep", &["30"], Path::new("/tmp"), &HashMap::new());
        command.kill_on_drop(true);
        let child = command.spawn().expect("spawn passthrough child");
        let pid = child.id().expect("child pid") as libc::pid_t;

        let pgid = unsafe { libc::getpgid(pid) };

        assert_eq!(pgid, pid, "child must be its own process-group leader");
    }

    /// kill_process_group must take the whole tree: tokio's kill_on_drop only
    /// signals the direct child, so a shell that forked a grandchild would
    /// leak it past a timeout without the group kill.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_process_group_reaps_grandchildren() {
        let sandbox = sandbox_without_backend(SandboxMode::Enabled, false);
        let mut command = sandbox.wrap(
            "sh",
            &["-c", "sleep 300 & echo $!; wait"],
            Path::new("/tmp"),
            &HashMap::new(),
        );
        command.kill_on_drop(true);
        command.stdout(std::process::Stdio::piped());
        let mut child = command.spawn().expect("spawn passthrough child");
        let pid = child.id().expect("child pid");

        // The shell prints the grandchild's pid as its first line.
        let mut stdout = child.stdout.take().expect("piped stdout");
        use tokio::io::AsyncBufReadExt as _;
        let mut line = String::new();
        tokio::io::BufReader::new(&mut stdout)
            .read_line(&mut line)
            .await
            .expect("read grandchild pid");
        let grandchild_pid: libc::pid_t = line.trim().parse().expect("pid line");
        assert_eq!(
            unsafe { libc::kill(grandchild_pid, 0) },
            0,
            "grandchild alive"
        );

        kill_process_group(pid);
        // Reap the direct child so the assert below races nothing.
        child.wait().await.expect("reap killed child");

        // The grandchild's parent died in the same volley, so init must adopt
        // and reap it — poll rather than assert instantly: a not-yet-reaped
        // zombie still answers kill(pid, 0).
        let mut reaped = false;
        for _ in 0..50 {
            if unsafe { libc::kill(grandchild_pid, 0) } == -1 {
                reaped = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        assert!(reaped, "grandchild must be gone after the group kill");
    }

    #[test]
    fn test_sandbox_mode_serialization() {
        #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
        struct ModeWrapper {
            mode: SandboxMode,
        }

        let enabled = toml::to_string(&ModeWrapper {
            mode: SandboxMode::Enabled,
        })
        .expect("serialize enabled mode");
        let disabled = toml::to_string(&ModeWrapper {
            mode: SandboxMode::Disabled,
        })
        .expect("serialize disabled mode");

        assert_eq!(enabled.trim(), "mode = \"enabled\"");
        assert_eq!(disabled.trim(), "mode = \"disabled\"");

        let enabled_roundtrip: ModeWrapper =
            toml::from_str(&enabled).expect("deserialize enabled mode");
        let disabled_roundtrip: ModeWrapper =
            toml::from_str(&disabled).expect("deserialize disabled mode");

        assert_eq!(
            enabled_roundtrip,
            ModeWrapper {
                mode: SandboxMode::Enabled
            }
        );
        assert_eq!(
            disabled_roundtrip,
            ModeWrapper {
                mode: SandboxMode::Disabled
            }
        );
    }
}
