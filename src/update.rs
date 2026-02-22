//! Update checking and Docker self-update.
//!
//! Checks GitHub releases for new versions and optionally performs
//! in-place container updates when the Docker socket is available.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use std::sync::Arc;
use std::time::Duration;

/// GitHub repository for release checks.
const GITHUB_REPO: &str = "spacedriveapp/spacebot";

/// Current binary version from Cargo.toml.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default check interval (1 hour).
const CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// Deployment environment, detected from SPACEBOT_DEPLOYMENT env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Deployment {
    Docker,
    /// Hosted on the Spacebot platform. Updates are managed by the platform
    /// via image rollouts — the instance itself cannot self-update.
    Hosted,
    Native,
}

impl Deployment {
    pub fn detect() -> Self {
        match std::env::var("SPACEBOT_DEPLOYMENT").as_deref() {
            Ok("docker") => Deployment::Docker,
            Ok("hosted") => Deployment::Hosted,
            _ => Deployment::Native,
        }
    }
}

/// Result of an update check.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
    pub release_notes: Option<String>,
    pub deployment: Deployment,
    /// Whether the container runtime socket is accessible (enables one-click update).
    pub can_apply: bool,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
    /// Resolved container socket path (not exposed in API responses).
    #[serde(skip)]
    pub socket_path: Option<String>,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self {
            current_version: CURRENT_VERSION.to_string(),
            latest_version: None,
            update_available: false,
            release_url: None,
            release_notes: None,
            deployment: Deployment::detect(),
            can_apply: false,
            checked_at: None,
            error: None,
            socket_path: None,
        }
    }
}

/// Shared update status, readable from API handlers.
pub type SharedUpdateStatus = Arc<ArcSwap<UpdateStatus>>;

pub fn new_shared_status() -> SharedUpdateStatus {
    let mut status = UpdateStatus::default();
    let socket = resolve_container_socket();
    status.can_apply = status.deployment == Deployment::Docker && socket.is_some();
    status.socket_path = socket;
    Arc::new(ArcSwap::from_pointee(status))
}

/// Minimal GitHub release response.
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
}

/// Check GitHub for the latest release and compare with current version.
pub async fn check_for_update(status: &SharedUpdateStatus) {
    let result = fetch_latest_release().await;

    let current = status.load();
    let mut next = UpdateStatus {
        current_version: CURRENT_VERSION.to_string(),
        deployment: current.deployment,
        can_apply: current.can_apply,
        socket_path: current.socket_path.clone(),
        checked_at: Some(chrono::Utc::now()),
        ..Default::default()
    };

    match result {
        Ok(release) => {
            let tag = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name);
            let is_newer = is_newer_version(tag, CURRENT_VERSION);

            next.latest_version = Some(tag.to_string());
            next.update_available = is_newer;
            next.release_url = Some(release.html_url);
            next.release_notes = release.body;

            if is_newer {
                tracing::info!(
                    current = CURRENT_VERSION,
                    latest = tag,
                    "new version available"
                );
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to check for updates");
            next.error = Some(error.to_string());
        }
    }

    status.store(Arc::new(next));
}

/// Spawn a background task that checks for updates periodically.
pub fn spawn_update_checker(status: SharedUpdateStatus) {
    tokio::spawn(async move {
        // Initial check after a short delay to not block startup
        tokio::time::sleep(Duration::from_secs(10)).await;
        check_for_update(&status).await;

        loop {
            tokio::time::sleep(CHECK_INTERVAL).await;
            check_for_update(&status).await;
        }
    });
}

/// Fetch the latest release from GitHub.
async fn fetch_latest_release() -> anyhow::Result<GitHubRelease> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );

    let client = reqwest::Client::builder()
        .user_agent(format!("spacebot/{}", CURRENT_VERSION))
        .timeout(Duration::from_secs(15))
        .build()?;

    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub API returned {}", response.status());
    }

    Ok(response.json().await?)
}

/// Compare two semver strings. Returns true if `latest` is newer than `current`.
fn is_newer_version(latest: &str, current: &str) -> bool {
    let Ok(latest) = semver::Version::parse(latest) else {
        return false;
    };
    let Ok(current) = semver::Version::parse(current) else {
        return false;
    };
    latest > current
}

/// Probe for a usable container runtime socket.
///
/// Checks in order:
/// 1. `DOCKER_HOST` env var (unix:// scheme only — tcp:// is skipped)
/// 2. `/var/run/docker.sock` (Docker rootful)
/// 3. `/run/podman/podman.sock` (Podman rootful)
/// 4. `$XDG_RUNTIME_DIR/podman/podman.sock` (Podman rootless)
///
/// Returns the path of the first socket that exists, or `None`.
fn resolve_container_socket() -> Option<String> {
    resolve_container_socket_with(
        std::env::var("DOCKER_HOST").ok().as_deref(),
        "/var/run/docker.sock",
        "/run/podman/podman.sock",
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
    )
}

fn resolve_container_socket_with(
    docker_host: Option<&str>,
    docker_rootful: &str,
    podman_rootful: &str,
    xdg_runtime_dir: Option<&str>,
) -> Option<String> {
    // 1. DOCKER_HOST env var (unix:// only — tcp:// and other schemes are skipped)
    if let Some(host) = docker_host {
        if let Some(path) = host.strip_prefix("unix://") {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
    }

    // 2. Docker rootful socket
    if std::path::Path::new(docker_rootful).exists() {
        return Some(docker_rootful.to_string());
    }

    // 3. Podman rootful socket
    if std::path::Path::new(podman_rootful).exists() {
        return Some(podman_rootful.to_string());
    }

    // 4. Podman rootless socket
    if let Some(xdg_runtime) = xdg_runtime_dir {
        let podman_rootless = format!("{}/podman/podman.sock", xdg_runtime);
        if std::path::Path::new(&podman_rootless).exists() {
            return Some(podman_rootless);
        }
    }

    None
}

/// Apply a Docker self-update: pull the new image, recreate this container.
///
/// This function does not return on success — the current container is stopped
/// and replaced. On failure it returns an error and the container keeps running.
pub async fn apply_docker_update(status: &SharedUpdateStatus) -> anyhow::Result<()> {
    let current = status.load();

    if !current.update_available {
        anyhow::bail!("no update available");
    }
    if current.deployment != Deployment::Docker {
        anyhow::bail!("not running in Docker");
    }
    if !current.can_apply {
        anyhow::bail!("Docker socket not available");
    }

    let socket_path = current
        .socket_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("container socket path not available"))?;

    let latest_version = current
        .latest_version
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no latest version"))?;

    tracing::info!(
        from = CURRENT_VERSION,
        to = latest_version,
        "applying Docker update"
    );

    let client_version = bollard::ClientVersion {
        major_version: 1,
        minor_version: 40,
    };
    let docker =
        bollard::Docker::connect_with_socket(socket_path, 120, &client_version)
            .map_err(|e| anyhow::anyhow!("failed to connect to Docker: {}", e))?;

    // Determine which image tag this container is running
    let container_id = get_own_container_id()?;
    let container_info = docker
        .inspect_container(&container_id, None)
        .await
        .map_err(|e| anyhow::anyhow!("failed to inspect container: {}", e))?;

    let current_image = container_info
        .config
        .as_ref()
        .and_then(|c| c.image.as_deref())
        .ok_or_else(|| anyhow::anyhow!("could not determine current image"))?
        .to_string();

    // Resolve the target image: same base name, new version tag.
    // e.g. ghcr.io/spacedriveapp/spacebot:v0.1.0-slim -> ghcr.io/spacedriveapp/spacebot:v0.2.0-slim
    let target_image = resolve_target_image(&current_image, latest_version);

    tracing::info!(
        current_image = %current_image,
        target_image = %target_image,
        "pulling new image"
    );

    // Pull the new image
    use bollard::image::CreateImageOptions;
    use futures::StreamExt as _;

    let pull_options = Some(CreateImageOptions {
        from_image: target_image.as_str(),
        ..Default::default()
    });

    let mut pull_stream = docker.create_image(pull_options, None, None);
    while let Some(result) = pull_stream.next().await {
        match result {
            Ok(info) => {
                if let Some(status) = &info.status {
                    tracing::debug!(status = %status, "pull progress");
                }
            }
            Err(error) => {
                anyhow::bail!("image pull failed: {}", error);
            }
        }
    }

    tracing::info!("image pulled, recreating container");

    // Recreate: create new container with same config but new image, then swap
    let container_name = container_info
        .name
        .as_deref()
        .map(|n| n.strip_prefix('/').unwrap_or(n))
        .ok_or_else(|| anyhow::anyhow!("container has no name"))?
        .to_string();

    let mut config = container_info
        .config
        .ok_or_else(|| anyhow::anyhow!("no container config"))?;

    config.image = Some(target_image.clone());

    // Preserve hostname if set
    if config.hostname.as_deref() == Some(&container_id) {
        config.hostname = None;
    }

    let host_config = container_info.host_config;
    let networking_config = container_info.network_settings.and_then(|ns| {
        let networks = ns.networks?;
        Some(bollard::container::NetworkingConfig {
            endpoints_config: networks,
        })
    });

    // Use a temporary name so we can swap atomically
    let temp_name = format!("{}-update", container_name);

    let create_options = bollard::container::CreateContainerOptions {
        name: temp_name.as_str(),
        ..Default::default()
    };

    let create_config = bollard::container::Config {
        image: config.image,
        env: config.env,
        cmd: config.cmd,
        entrypoint: config.entrypoint,
        working_dir: config.working_dir,
        exposed_ports: config.exposed_ports,
        volumes: config.volumes,
        labels: config.labels,
        host_config,
        networking_config,
        ..Default::default()
    };

    let new_container = docker
        .create_container(Some(create_options), create_config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create new container: {}", e))?;

    tracing::info!(new_id = %new_container.id, "new container created");

    // Stop the current container (this process will be killed)
    // The rename + start happens from a brief window where we stop ourselves.
    // To handle this, we rename the old container first, then start the new one,
    // then stop ourselves. The new container takes over.

    // Rename current container out of the way
    let old_name = format!("{}-old", container_name);
    docker
        .rename_container(
            &container_id,
            bollard::container::RenameContainerOptions { name: &old_name },
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to rename old container: {}", e))?;

    // Rename new container to the original name
    docker
        .rename_container(
            &new_container.id,
            bollard::container::RenameContainerOptions {
                name: &container_name,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to rename new container: {}", e))?;

    // Start the new container
    docker
        .start_container::<String>(&new_container.id, None)
        .await
        .map_err(|e| anyhow::anyhow!("failed to start new container: {}", e))?;

    tracing::info!("new container started, stopping old container");

    // Stop the old container (ourselves). This process will terminate.
    docker
        .stop_container(
            &container_id,
            Some(bollard::container::StopContainerOptions { t: 10 }),
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to stop old container: {}", e))?;

    // Remove the old container after stop
    docker
        .remove_container(
            &container_id,
            Some(bollard::container::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
        .ok(); // Best effort — we're shutting down

    // We shouldn't reach here since stop_container kills us,
    // but just in case:
    std::process::exit(0);
}

/// Read this container's ID from /proc/self/mountinfo or the hostname.
fn get_own_container_id() -> anyhow::Result<String> {
    // Try /proc/self/mountinfo first — works for both Docker and Podman
    if let Ok(content) = std::fs::read_to_string("/proc/self/mountinfo") {
        if let Some(id) = parse_container_id_from_mountinfo(&content) {
            return Ok(id);
        }
    }

    // Fall back to the hostname — Docker sets it to the short container ID
    if let Ok(hostname) = std::fs::read_to_string("/etc/hostname") {
        let hostname = hostname.trim();
        if hostname.len() >= 12 && hostname.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(hostname.to_string());
        }
    }

    anyhow::bail!("could not determine own container ID")
}

/// Parse a 64-character hex container ID from /proc/self/mountinfo content.
///
/// Recognises both Docker (`/docker/containers/<id>/`) and Podman
/// (`/overlay-containers/<id>/`) storage path patterns.
fn parse_container_id_from_mountinfo(content: &str) -> Option<String> {
    for (marker, offset) in &[
        ("/docker/containers/", 19usize),
        ("/overlay-containers/", 20usize),
    ] {
        for line in content.lines() {
            if let Some(pos) = line.find(marker) {
                let after = &line[pos + offset..];
                if let Some(end) = after.find('/') {
                    let id = &after[..end];
                    if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Some(id.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Given a current image reference and a new version, produce the target image tag.
///
/// Examples:
///   - `ghcr.io/spacedriveapp/spacebot:v0.1.0-slim` + `0.2.0` -> `ghcr.io/spacedriveapp/spacebot:v0.2.0-slim`
///   - `ghcr.io/spacedriveapp/spacebot:slim` + `0.2.0` -> `ghcr.io/spacedriveapp/spacebot:v0.2.0-slim`
///   - `ghcr.io/spacedriveapp/spacebot:latest` + `0.2.0` -> `ghcr.io/spacedriveapp/spacebot:v0.2.0-slim`
fn resolve_target_image(current_image: &str, new_version: &str) -> String {
    let (base, tag) = match current_image.rsplit_once(':') {
        Some((b, t)) => (b, t),
        None => (current_image, "latest"),
    };

    // Determine the variant suffix (slim, full, or default to slim)
    let variant = if tag.contains("full") { "full" } else { "slim" };

    format!("{}:v{}-{}", base, new_version, variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("0.2.0", "0.1.0"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
    }

    #[test]
    fn test_resolve_target_image() {
        assert_eq!(
            resolve_target_image("ghcr.io/spacedriveapp/spacebot:v0.1.0-slim", "0.2.0"),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-slim"
        );
        assert_eq!(
            resolve_target_image("ghcr.io/spacedriveapp/spacebot:v0.1.0-full", "0.2.0"),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-full"
        );
        assert_eq!(
            resolve_target_image("ghcr.io/spacedriveapp/spacebot:latest", "0.2.0"),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-slim"
        );
        assert_eq!(
            resolve_target_image("ghcr.io/spacedriveapp/spacebot:slim", "0.2.0"),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-slim"
        );
    }

    // ── resolve_container_socket_with ─────────────────────────────────────

    fn fake_socket(dir: &tempfile::TempDir, name: &str) -> String {
        let path = dir.path().join(name);
        std::fs::File::create(&path).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn test_resolve_socket_docker_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let docker = fake_socket(&dir, "docker.sock");
        let result = resolve_container_socket_with(None, &docker, "/nonexistent", None);
        assert_eq!(result.as_deref(), Some(docker.as_str()));
    }

    #[test]
    fn test_resolve_socket_podman_rootful_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let podman = fake_socket(&dir, "podman.sock");
        let result = resolve_container_socket_with(None, "/nonexistent", &podman, None);
        assert_eq!(result.as_deref(), Some(podman.as_str()));
    }

    #[test]
    fn test_resolve_socket_podman_rootless() {
        let dir = tempfile::TempDir::new().unwrap();
        let xdg = dir.path().to_str().unwrap().to_string();
        std::fs::create_dir_all(dir.path().join("podman")).unwrap();
        let sock = fake_socket(&dir, "podman/podman.sock");
        let result =
            resolve_container_socket_with(None, "/nonexistent", "/nonexistent", Some(&xdg));
        assert_eq!(result, Some(sock));
    }

    #[test]
    fn test_resolve_socket_docker_host_unix() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock = fake_socket(&dir, "custom.sock");
        let host = format!("unix://{}", sock);
        let result =
            resolve_container_socket_with(Some(&host), "/nonexistent", "/nonexistent", None);
        assert_eq!(result.as_deref(), Some(sock.as_str()));
    }

    #[test]
    fn test_resolve_socket_docker_host_tcp_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let docker = fake_socket(&dir, "docker.sock");
        // tcp:// is skipped; falls through to the docker_rootful path
        let result = resolve_container_socket_with(
            Some("tcp://192.168.1.1:2376"),
            &docker,
            "/nonexistent",
            None,
        );
        assert_eq!(result.as_deref(), Some(docker.as_str()));
    }

    #[test]
    fn test_resolve_socket_none() {
        let result =
            resolve_container_socket_with(None, "/nonexistent", "/nonexistent", None);
        assert_eq!(result, None);
    }

    // ── parse_container_id_from_mountinfo ─────────────────────────────────

    const DOCKER_ID: &str =
        "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    const PODMAN_ID: &str =
        "0011223344556677889900112233445566778899001122334455667788990011";

    #[test]
    fn test_parse_mountinfo_docker() {
        let content = format!(
            "123 0 0:1 / / rw - ext4 /dev/sda rw\n\
             456 123 0:2 / /var/lib/docker/containers/{}/shm rw\n",
            DOCKER_ID
        );
        assert_eq!(
            parse_container_id_from_mountinfo(&content).as_deref(),
            Some(DOCKER_ID)
        );
    }

    #[test]
    fn test_parse_mountinfo_podman() {
        let content = format!(
            "123 0 0:1 / / rw - ext4 /dev/sda rw\n\
             456 123 0:2 / /var/lib/containers/storage/overlay-containers/{}/userdata rw\n",
            PODMAN_ID
        );
        assert_eq!(
            parse_container_id_from_mountinfo(&content).as_deref(),
            Some(PODMAN_ID)
        );
    }

    #[test]
    fn test_parse_mountinfo_no_match() {
        let content = "123 0 0:1 / / rw - ext4 /dev/sda rw\n";
        assert_eq!(parse_container_id_from_mountinfo(content), None);
    }

    #[test]
    fn test_parse_mountinfo_short_id_ignored() {
        // A path segment that looks like a container path but has a short ID
        let content = "456 123 0:2 / /docker/containers/abc123/shm rw\n";
        assert_eq!(parse_container_id_from_mountinfo(content), None);
    }
}
