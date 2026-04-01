use tokio::process::Command;
use tracing::{debug, info, warn};

fn bubblewrap_true_binary() -> &'static str {
    if std::path::Path::new("/bin/true").exists() {
        "/bin/true"
    } else {
        "/usr/bin/true"
    }
}

/// Available sandbox backends detected at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    /// Linux bubblewrap (bwrap) backend.
    Bubblewrap,
    /// macOS sandbox-exec backend.
    SandboxExec,
    /// No sandbox backend available.
    None,
}

/// Detect available sandbox backend by probing system binaries.
/// Runs preflight checks to ensure the backend actually works.
pub async fn detect_backend() -> SandboxBackend {
    // Try Linux bubblewrap first
    match check_bubblewrap().await {
        Ok(probe) if probe.exists => {
            info!(
                proc_supported = probe.proc_supported,
                "Sandbox backend: bubblewrap"
            );
            return SandboxBackend::Bubblewrap;
        }
        Ok(_) => {
            debug!("bubblewrap not available");
        }
        Err(error) => {
            warn!(error = %error, "bubblewrap probe failed");
        }
    }

    // Try macOS sandbox-exec
    match check_sandbox_exec().await {
        Ok(true) => {
            info!("Sandbox backend: sandbox-exec");
            return SandboxBackend::SandboxExec;
        }
        Ok(false) => {
            debug!("sandbox-exec not available");
        }
        Err(error) => {
            warn!(error = %error, "sandbox-exec probe failed");
        }
    }

    warn!("No sandbox backend available - commands will run unsandboxed");
    SandboxBackend::None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BubblewrapProbe {
    exists: bool,
    proc_supported: bool,
}

async fn check_bubblewrap() -> Result<BubblewrapProbe, Box<dyn std::error::Error>> {
    // Check if bwrap exists
    let version_check = match Command::new("bwrap").arg("--version").output().await {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BubblewrapProbe {
                exists: false,
                proc_supported: false,
            });
        }
        Err(error) => return Err(Box::new(error)),
    };

    if !version_check.status.success() {
        return Ok(BubblewrapProbe {
            exists: false,
            proc_supported: false,
        });
    }

    // Run preflight: try to use --proc flag (may fail in nested containers)
    let preflight = Command::new("bwrap")
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
        .await?;

    Ok(BubblewrapProbe {
        exists: true,
        proc_supported: preflight.status.success(),
    })
}

async fn check_sandbox_exec() -> Result<bool, Box<dyn std::error::Error>> {
    // Check if sandbox-exec exists at the hardcoded system path
    match tokio::fs::metadata("/usr/bin/sandbox-exec").await {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Box::new(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_backend_detection() {
        // This test just verifies the function exists and returns a value
        // Actual detection depends on the host environment
        let _backend = detect_backend().await;
        // Should not panic
    }

    #[test]
    fn test_sandbox_backend_variants() {
        let bubblewrap = SandboxBackend::Bubblewrap;
        let sandbox_exec = SandboxBackend::SandboxExec;
        let none = SandboxBackend::None;

        // Verify all variants exist
        assert!(matches!(bubblewrap, SandboxBackend::Bubblewrap));
        assert!(matches!(sandbox_exec, SandboxBackend::SandboxExec));
        assert!(matches!(none, SandboxBackend::None));
    }
}
