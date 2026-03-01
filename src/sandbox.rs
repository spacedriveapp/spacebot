//! OS-level filesystem containment for shell and exec tool subprocesses.
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
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::Enabled,
            writable_paths: Vec::new(),
            passthrough_env: Vec::new(),
        }
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

/// Detected sandbox backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxBackend {
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
/// deterministic sandbox-local paths.
const RESERVED_ENV_VARS: &[&str] = &["PATH", "HOME", "TMPDIR"];

/// Returns true if the variable name is reserved (set by hardened defaults) or
/// is in the safe-vars list, and therefore must not be overridden by
/// `passthrough_env`.
fn is_reserved_env_var(name: &str) -> bool {
    RESERVED_ENV_VARS.contains(&name) || SAFE_ENV_VARS.contains(&name)
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
    backend: SandboxBackend,
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
    ) -> Self {
        let tools_bin = instance_dir.join("tools/bin");

        // Always detect the backend so we know what's available if the user
        // later enables sandboxing via the API.
        let backend = detect_backend().await;
        let current_mode = config.load().mode;

        match backend {
            SandboxBackend::Bubblewrap { proc_supported } => {
                if current_mode == SandboxMode::Enabled {
                    tracing::info!(proc_supported, "sandbox enabled: bubblewrap backend");
                } else {
                    tracing::info!(
                        proc_supported,
                        "sandbox disabled by config (bubblewrap available)"
                    );
                }
            }
            SandboxBackend::SandboxExec => {
                if current_mode == SandboxMode::Enabled {
                    tracing::info!("sandbox enabled: macOS sandbox-exec backend");
                } else {
                    tracing::info!("sandbox disabled by config (sandbox-exec available)");
                }
            }
            SandboxBackend::None if current_mode == SandboxMode::Enabled => {
                tracing::warn!(
                    "sandbox mode is enabled but no backend available — \
                     processes will run unsandboxed"
                );
            }
            SandboxBackend::None => {
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

    /// Read tool secrets from the store for injection into subprocess environment.
    fn tool_secrets(&self) -> HashMap<String, String> {
        let guard = self.secrets_store.load();
        match guard.as_ref() {
            Some(store) => store.tool_env_vars(),
            None => HashMap::new(),
        }
    }

    /// True when sandbox mode is enabled in config.
    pub fn mode_enabled(&self) -> bool {
        self.config.load().mode == SandboxMode::Enabled
    }

    /// True when OS-level containment is currently active.
    ///
    /// If mode is enabled but no backend is available, this returns false
    /// because subprocesses fall back to passthrough execution.
    pub fn containment_active(&self) -> bool {
        self.mode_enabled() && !matches!(self.backend, SandboxBackend::None)
    }

    /// Read-allowlisted filesystem paths exposed to shell/exec subprocesses when
    /// containment is active.
    pub fn prompt_read_allowlist(&self) -> Vec<String> {
        if !self.containment_active() {
            return Vec::new();
        }

        let config = self.config.load();
        let mut paths = Vec::new();

        match self.backend {
            SandboxBackend::Bubblewrap { .. } => {
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

                for path in &config.writable_paths {
                    if let Ok(canonical) = path.canonicalize() {
                        push_unique_path(&mut paths, canonical);
                    }
                }
            }
            SandboxBackend::SandboxExec => {
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

                for path in &config.writable_paths {
                    push_unique_path(&mut paths, canonicalize_or_self(path));
                }
            }
            SandboxBackend::None => {}
        }

        paths
    }

    /// Write-allowlisted filesystem paths exposed to shell/exec subprocesses when
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
            SandboxBackend::Bubblewrap { .. } => {
                for path in &config.writable_paths {
                    if let Ok(canonical) = path.canonicalize() {
                        push_unique_path(&mut paths, canonical);
                    }
                }
            }
            SandboxBackend::SandboxExec => {
                for path in &config.writable_paths {
                    push_unique_path(&mut paths, canonicalize_or_self(path));
                }
            }
            SandboxBackend::None => {}
        }

        paths
    }

    /// Wrap a command for sandboxed execution.
    ///
    /// Returns a `Command` ready to spawn, potentially prefixed with bwrap or
    /// sandbox-exec depending on the detected backend. The caller still needs
    /// to set stdout/stderr/timeout after this call.
    ///
    /// Reads the current `SandboxMode` from the shared `ArcSwap<SandboxConfig>`
    /// on every call, so changes via the API take effect immediately.
    pub fn wrap(&self, program: &str, args: &[&str], working_dir: &Path) -> Command {
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
            );
        }

        match self.backend {
            SandboxBackend::Bubblewrap { proc_supported } => self.wrap_bubblewrap(
                program,
                args,
                working_dir,
                proc_supported,
                &path_env,
                &config,
                &tool_secrets,
            ),
            SandboxBackend::SandboxExec => self.wrap_sandbox_exec(
                program,
                args,
                working_dir,
                &path_env,
                &config,
                &tool_secrets,
            ),
            SandboxBackend::None => self.wrap_passthrough(
                program,
                args,
                working_dir,
                &path_env,
                &config,
                &tool_secrets,
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

        // 6. Each configured writable path (canonicalized dynamically)
        for path in &config.writable_paths {
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

        // 15. Worker keyring isolation (Linux) — give the child a fresh empty
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

        // 16. The actual command
        cmd.arg("--").arg(program);
        for arg in args {
            cmd.arg(arg);
        }

        cmd
    }

    /// macOS: wrap with sandbox-exec and a generated SBPL profile.
    fn wrap_sandbox_exec(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        path_env: &str,
        config: &SandboxConfig,
        tool_secrets: &HashMap<String, String>,
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
        for var_name in SAFE_ENV_VARS {
            if let Ok(value) = std::env::var(var_name) {
                cmd.env(var_name, value);
            }
        }
        // Inject tool secrets from the secret store.
        for (name, value) in tool_secrets {
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

        cmd
    }

    /// No backend: pass through without OS-level containment.
    ///
    /// Still applies environment sanitization — workers never inherit the full
    /// parent environment regardless of sandbox state.
    fn wrap_passthrough(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        path_env: &str,
        config: &SandboxConfig,
        tool_secrets: &HashMap<String, String>,
    ) -> Command {
        let mut cmd = Command::new(program);
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
        for var_name in SAFE_ENV_VARS {
            if let Ok(value) = std::env::var(var_name) {
                cmd.env(var_name, value);
            }
        }
        // Inject tool secrets from the secret store.
        for (name, value) in tool_secrets {
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

        // Additional writable paths (canonicalized dynamically) are readable and writable.
        for (index, path) in config.writable_paths.iter().enumerate() {
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
  (global-name "com.apple.system.opendirectoryd.libinfo"))
(allow ipc-posix-sem)
(allow pseudo-tty)
(allow network*)
"#,
        );

        profile
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
async fn detect_backend() -> SandboxBackend {
    if cfg!(target_os = "linux") {
        detect_bubblewrap().await
    } else if cfg!(target_os = "macos") {
        detect_sandbox_exec()
    } else {
        tracing::warn!("no sandbox backend available for this platform");
        SandboxBackend::None
    }
}

/// Linux: check if bwrap is available and whether --proc /proc works.
async fn detect_bubblewrap() -> SandboxBackend {
    // Check if bwrap exists
    let version_check = Command::new("bwrap").arg("--version").output().await;

    if version_check.is_err() {
        tracing::debug!("bwrap not found in PATH");
        return SandboxBackend::None;
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
            "/usr/bin/true",
        ])
        .output()
        .await;

    let proc_supported = proc_check.is_ok_and(|output| output.status.success());

    if !proc_supported {
        tracing::debug!("bwrap --proc /proc not supported, running without fresh procfs");
    }

    SandboxBackend::Bubblewrap { proc_supported }
}

/// macOS: check if sandbox-exec exists at its known path.
fn detect_sandbox_exec() -> SandboxBackend {
    if Path::new("/usr/bin/sandbox-exec").exists() {
        SandboxBackend::SandboxExec
    } else {
        tracing::debug!("/usr/bin/sandbox-exec not found");
        SandboxBackend::None
    }
}
