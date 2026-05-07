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
    #[serde(default)]
    pub readable_paths: Vec<PathBuf>,
    /// Environment variable names to forward from the parent process into worker
    /// subprocesses. This is the escape hatch for self-hosted users who set env
    /// vars in Docker/systemd but don't configure a secret store. When the secret
    /// store is available, `passthrough_env` is redundant — everything should be
    /// in the store. The field is additive either way.
    #[serde(default)]
    pub passthrough_env: Vec<String>,
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
            readable_paths: Vec::new(),
            passthrough_env: Vec::new(),
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
                tracing::warn!(
                    "sandbox mode is enabled but no backend available — \
                     processes will run unsandboxed"
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
    pub fn mode_enabled(&self) -> bool {
        self.config.load().mode == SandboxMode::Enabled
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
    /// allowed path (workspace, writable paths, or readable paths).
    ///
    /// Used by shell/file tools to relax the workspace boundary when
    /// project paths are registered.
    pub fn is_path_allowed(&self, canonical: &Path) -> bool {
        let workspace_canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let data_dir_canonical = self
            .data_dir
            .canonicalize()
            .unwrap_or_else(|_| self.data_dir.clone());

        if canonical.starts_with(&data_dir_canonical) {
            return false;
        }

        if canonical.starts_with(&workspace_canonical) {
            return true;
        }

        let config = self.config.load();
        for path in config.all_writable_paths() {
            let allowed = path.canonicalize().unwrap_or_else(|_| path.clone());
            if allowed.starts_with(&data_dir_canonical) {
                continue;
            }
            if canonical.starts_with(&allowed) {
                return true;
            }
        }
        for path in &config.readable_paths {
            let allowed = path.canonicalize().unwrap_or_else(|_| path.clone());
            if allowed.starts_with(&data_dir_canonical) {
                continue;
            }
            if canonical.starts_with(&allowed) {
                return !canonical.starts_with(&data_dir_canonical);
            }
        }
        false
    }

    /// Check whether a canonical path is writable under sandbox policy.
    pub fn is_path_writable(&self, canonical: &Path) -> bool {
        if !self.is_path_allowed(canonical) {
            return false;
        }
        let config = self.config.load();
        for path in &config.readable_paths {
            let allowed = path.canonicalize().unwrap_or_else(|_| path.clone());
            if canonical.starts_with(&allowed) {
                return false;
            }
        }
        true
    }

    /// True when OS-level containment is currently active.
    ///
    /// If mode is enabled but no backend is available, this returns false
    /// because subprocesses fall back to passthrough execution.
    pub fn containment_active(&self) -> bool {
        self.mode_enabled() && !matches!(self.backend, InternalBackend::None)
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

                for path in &config.readable_paths {
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

                for path in &config.readable_paths {
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
        let tool_secrets = self.tool_secrets();

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

        // 7. User-configured read-only paths
        for path in &config.readable_paths {
            match path.canonicalize() {
                Ok(canonical) => {
                    cmd.arg("--ro-bind").arg(&canonical).arg(&canonical);
                }
                Err(error) => {
                    tracing::debug!(
                        path = %path.display(),
                        %error,
                        "skipping readable_path (does not exist or is unresolvable)"
                    );
                }
            }
        }

        // 8. Mask agent data dir with an empty tmpfs to prevent reads/writes,
        // even when it overlaps with workspace-related paths.
        cmd.arg("--tmpfs").arg(&self.data_dir);

        // 9. Isolation flags
        cmd.arg("--unshare-pid");
        cmd.arg("--new-session");
        cmd.arg("--die-with-parent");

        // 10. Clear all inherited environment variables. Workers must not see
        // system secrets (LLM API keys, messaging tokens) or SPACEBOT_* internals.
        cmd.arg("--clearenv");

        // 11. Working directory
        cmd.arg("--chdir").arg(working_dir);

        // 12. Set PATH inside the sandbox
        cmd.arg("--setenv").arg("PATH").arg(path_env);

        // 13. Set deterministic sandbox-local home/temp paths.
        cmd.arg("--setenv")
            .arg("HOME")
            .arg(self.workspace.to_string_lossy().into_owned());
        cmd.arg("--setenv").arg("TMPDIR").arg("/tmp");

        // 13a. Suppress interactive prompts. CI=true prevents npm/npx/yarn
        // from prompting; DEBIAN_FRONTEND=noninteractive prevents apt-get.
        // Shell tool runs without stdin, so interactive prompts always hang.
        cmd.arg("--setenv").arg("CI").arg("true");
        cmd.arg("--setenv")
            .arg("DEBIAN_FRONTEND")
            .arg("noninteractive");

        // 14. Re-inject safe environment variables for basic process operation
        for var_name in SAFE_ENV_VARS {
            if let Ok(value) = std::env::var(var_name) {
                cmd.arg("--setenv").arg(var_name).arg(value);
            }
        }

        // 15. Re-inject tool secrets from the secret store.
        // Only tool-category secrets are injected; system secrets (LLM API keys,
        // messaging tokens) never enter subprocess environments.
        for (name, value) in tool_secrets {
            if is_reserved_env_var(name) {
                tracing::debug!(%name, "skipping reserved tool secret name");
                continue;
            }
            cmd.arg("--setenv").arg(name).arg(value);
        }

        // 16. Re-inject passthrough env vars (user-configured forwarding),
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

        // 17. Per-command env vars from tool caller (e.g. shell tool `env`).
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

        // 18. Worker keyring isolation (Linux) — give the child a fresh empty
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

        // 19. The actual command
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

        for (index, path) in config.readable_paths.iter().enumerate() {
            let canonical = canonicalize_or_self(path);
            profile.push_str(&format!(
                "; readable path {index}\n(allow file-read* (subpath \"{}\"))\n(deny file-write* (subpath \"{}\"))\n",
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

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwap;
    use std::sync::Arc;

    fn sandbox_for_path_tests(
        workspace: PathBuf,
        data_dir: PathBuf,
        readable_paths: Vec<PathBuf>,
        writable_paths: Vec<PathBuf>,
    ) -> Sandbox {
        let config = Arc::new(ArcSwap::from_pointee(SandboxConfig {
            mode: SandboxMode::Enabled,
            readable_paths,
            writable_paths,
            passthrough_env: Vec::new(),
            project_paths: Vec::new(),
        }));

        Sandbox {
            workspace,
            data_dir,
            backend: InternalBackend::None,
            config,
            tools_bin: PathBuf::from("/nonexistent/tools-bin"),
            secrets_store: ArcSwap::from_pointee(None),
        }
    }

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.mode, SandboxMode::Enabled);
        assert!(config.writable_paths.is_empty());
        assert!(config.readable_paths.is_empty());
        assert!(config.project_paths.is_empty());
        assert!(config.passthrough_env.is_empty());
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

    #[test]
    fn readable_paths_are_allowed_for_reads_but_not_writes() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let readable = temp_dir.path().join("readonly");
        let data_dir = temp_dir.path().join("data");

        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&readable).expect("create readable");
        std::fs::create_dir_all(&data_dir).expect("create data dir");

        let sandbox = sandbox_for_path_tests(
            workspace,
            data_dir,
            vec![readable.clone()],
            Vec::new(),
        );

        let readable_file = readable.join("doc.txt");
        std::fs::write(&readable_file, "hello").expect("write readable file");
        let canonical = readable_file.canonicalize().expect("canonicalize readable file");

        assert!(sandbox.is_path_allowed(&canonical));
        assert!(!sandbox.is_path_writable(&canonical));
    }

    #[test]
    fn readable_paths_do_not_override_data_dir_protection() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let data_dir = temp_dir.path().join("agent-data");

        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&data_dir).expect("create data dir");

        let sandbox = sandbox_for_path_tests(
            workspace,
            data_dir.clone(),
            vec![temp_dir.path().to_path_buf()],
            Vec::new(),
        );

        let secret = data_dir.join("secret.txt");
        std::fs::write(&secret, "secret").expect("write secret file");
        let canonical = secret.canonicalize().expect("canonicalize secret file");

        assert!(!sandbox.is_path_allowed(&canonical));
        assert!(!sandbox.is_path_writable(&canonical));
    }
}
