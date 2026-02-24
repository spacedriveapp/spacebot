//! OS-level filesystem containment for shell and exec tool subprocesses.
//!
//! Replaces string-based command filtering with kernel-enforced boundaries.
//! On Linux, uses bubblewrap (bwrap) for mount namespace isolation.
//! On macOS, uses sandbox-exec with a generated SBPL profile.
//! Falls back to no sandboxing when neither backend is available.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Sandbox configuration from the agent config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    #[serde(default = "default_mode")]
    pub mode: SandboxMode,
    #[serde(default)]
    pub writable_paths: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::Enabled,
            writable_paths: Vec::new(),
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

/// Filesystem sandbox for subprocess execution.
///
/// Created once per agent at startup, shared via `Arc` across all workers.
/// Wraps `tokio::process::Command` to apply OS-level containment before spawning.
#[derive(Debug, Clone)]
pub struct Sandbox {
    #[allow(dead_code)]
    mode: SandboxMode,
    workspace: PathBuf,
    data_dir: PathBuf,
    tools_bin: PathBuf,
    writable_paths: Vec<PathBuf>,
    backend: SandboxBackend,
}

impl Sandbox {
    /// Create a sandbox with the given configuration. Probes for backend support.
    pub async fn new(
        config: &SandboxConfig,
        workspace: PathBuf,
        instance_dir: &Path,
        data_dir: PathBuf,
    ) -> Self {
        let tools_bin = instance_dir.join("tools/bin");
        let backend = if config.mode == SandboxMode::Disabled {
            SandboxBackend::None
        } else {
            detect_backend().await
        };

        match backend {
            SandboxBackend::Bubblewrap { proc_supported } => {
                tracing::info!(proc_supported, "sandbox enabled: bubblewrap backend");
            }
            SandboxBackend::SandboxExec => {
                tracing::info!("sandbox enabled: macOS sandbox-exec backend");
            }
            SandboxBackend::None if config.mode == SandboxMode::Enabled => {
                tracing::warn!(
                    "sandbox mode is enabled but no backend available — \
                     processes will run unsandboxed"
                );
            }
            SandboxBackend::None => {
                tracing::info!("sandbox disabled by config");
            }
        }

        // Canonicalize paths at construction to resolve symlinks and validate existence.
        let workspace = canonicalize_or_self(&workspace);
        let data_dir = canonicalize_or_self(&data_dir);
        let writable_paths: Vec<PathBuf> = config
            .writable_paths
            .iter()
            .filter_map(|path| match path.canonicalize() {
                Ok(canonical) => Some(canonical),
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "dropping writable_path entry (does not exist or is unresolvable)"
                    );
                    None
                }
            })
            .collect();

        Self {
            mode: config.mode,
            workspace,
            data_dir,
            tools_bin,
            writable_paths,
            backend,
        }
    }

    /// Wrap a command for sandboxed execution.
    ///
    /// Returns a `Command` ready to spawn, potentially prefixed with bwrap or
    /// sandbox-exec depending on the detected backend. The caller still needs
    /// to set stdout/stderr/timeout after this call.
    pub fn wrap(&self, program: &str, args: &[&str], working_dir: &Path) -> Command {
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

        match self.backend {
            SandboxBackend::Bubblewrap { proc_supported } => {
                self.wrap_bubblewrap(program, args, working_dir, proc_supported, &path_env)
            }
            SandboxBackend::SandboxExec => {
                self.wrap_sandbox_exec(program, args, working_dir, &path_env)
            }
            SandboxBackend::None => self.wrap_passthrough(program, args, working_dir, &path_env),
        }
    }

    /// Linux: wrap with bubblewrap mount namespace.
    fn wrap_bubblewrap(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        proc_supported: bool,
        path_env: &str,
    ) -> Command {
        let mut cmd = Command::new("bwrap");

        // Mount order matters — later mounts override earlier ones.
        // 1. Entire filesystem read-only
        cmd.arg("--ro-bind").arg("/").arg("/");

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

        // 6. Each configured writable path
        for writable in &self.writable_paths {
            cmd.arg("--bind").arg(writable).arg(writable);
        }

        // 7. Re-protect agent data dir (may overlap with workspace parent)
        cmd.arg("--ro-bind").arg(&self.data_dir).arg(&self.data_dir);

        // 8. Isolation flags
        cmd.arg("--unshare-pid");
        cmd.arg("--new-session");
        cmd.arg("--die-with-parent");

        // 9. Working directory
        cmd.arg("--chdir").arg(working_dir);

        // 10. Set PATH inside the sandbox
        cmd.arg("--setenv").arg("PATH").arg(path_env);

        // 11. The actual command
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
    ) -> Command {
        let profile = self.generate_sbpl_profile();

        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-p").arg(profile);
        cmd.arg(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.current_dir(working_dir);
        cmd.env("PATH", path_env);

        cmd
    }

    /// No backend: pass through unchanged.
    fn wrap_passthrough(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        path_env: &str,
    ) -> Command {
        let mut cmd = Command::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.current_dir(working_dir);
        cmd.env("PATH", path_env);

        cmd
    }

    /// Generate a macOS SBPL (Sandbox Profile Language) policy.
    ///
    /// Paths are canonicalized because /var on macOS is actually /private/var.
    fn generate_sbpl_profile(&self) -> String {
        let workspace = canonicalize_or_self(&self.workspace);

        let mut profile = String::from(
            r#"(version 1)
(deny default)

; process basics
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))

; filesystem: read everything
(allow file-read*)

"#,
        );

        // Workspace writable
        profile.push_str(&format!(
            "; workspace writable\n(allow file-write* (subpath \"{}\"))\n\n",
            workspace.display()
        ));

        // Additional writable paths
        for (index, writable) in self.writable_paths.iter().enumerate() {
            let canonical = canonicalize_or_self(writable);
            profile.push_str(&format!(
                "; writable path {index}\n(allow file-write* (subpath \"{}\"))\n",
                canonical.display()
            ));
        }

        // Protect data_dir even if it falls under the workspace subtree
        let data_dir = canonicalize_or_self(&self.data_dir);
        profile.push_str(&format!(
            "\n; data dir read-only\n(deny file-write* (subpath \"{}\"))\n",
            data_dir.display()
        ));

        // /tmp writable
        let tmp = canonicalize_or_self(Path::new("/tmp"));
        profile.push_str(&format!(
            "\n; tmp writable\n(allow file-write* (subpath \"{}\"))\n",
            tmp.display()
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
