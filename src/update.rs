//! Update checking and container self-update.
//!
//! Checks GitHub releases for new versions and optionally performs
//! in-place container updates when a runtime socket is available.

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

const DOCKER_ROOTFUL_SOCKET: &str = "/var/run/docker.sock";
const PODMAN_ROOTFUL_SOCKET: &str = "/run/podman/podman.sock";

fn container_runtime_socket_hint() -> String {
    format!(
        "Mount a container runtime socket to enable one-click updates (Docker: {DOCKER_ROOTFUL_SOCKET}, Podman: {PODMAN_ROOTFUL_SOCKET}, Podman rootless: $XDG_RUNTIME_DIR/podman/podman.sock -> {PODMAN_ROOTFUL_SOCKET})."
    )
}

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
            _ if is_running_in_container() => Deployment::Docker,
            _ => Deployment::Native,
        }
    }
}

fn is_running_in_container() -> bool {
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }

    let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") else {
        return false;
    };

    ["docker", "containerd", "kubepods", "podman"]
        .iter()
        .any(|marker| cgroup.contains(marker))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageVariant {
    Slim,
    Full,
}

impl ImageVariant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Slim => "slim",
            Self::Full => "full",
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
    /// Whether a container runtime socket is accessible (enables one-click update).
    pub can_apply: bool,
    /// Human-readable reason when one-click apply is unavailable.
    pub cannot_apply_reason: Option<String>,
    /// Current container image reference when running in Docker.
    pub docker_image: Option<String>,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
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
            cannot_apply_reason: None,
            docker_image: None,
            checked_at: None,
            error: None,
        }
    }
}

/// Shared update status, readable from API handlers.
pub type SharedUpdateStatus = Arc<ArcSwap<UpdateStatus>>;

pub fn new_shared_status() -> SharedUpdateStatus {
    let mut status = UpdateStatus::default();
    match status.deployment {
        Deployment::Docker => {
            status.can_apply = resolve_container_socket().is_some();
            if !status.can_apply {
                status.cannot_apply_reason = Some(container_runtime_socket_hint());
            }
        }
        Deployment::Native => {
            status.cannot_apply_reason =
                Some("Native/source installs update manually (rebuild + restart).".to_string());
        }
        Deployment::Hosted => {
            status.cannot_apply_reason = Some(
                "Hosted instances are updated by platform rollout, not self-service.".to_string(),
            );
        }
    }
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
    let capability = detect_apply_capability(current.deployment).await;
    let mut next = UpdateStatus {
        current_version: CURRENT_VERSION.to_string(),
        deployment: current.deployment,
        can_apply: capability.can_apply,
        cannot_apply_reason: capability.cannot_apply_reason,
        docker_image: capability.docker_image,
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
/// 1. `DOCKER_HOST` env var (`unix://` only)
/// 2. Docker rootful socket (`/var/run/docker.sock`)
/// 3. Podman rootful socket (`/run/podman/podman.sock`)
/// 4. Podman rootless socket (`$XDG_RUNTIME_DIR/podman/podman.sock`)
fn resolve_container_socket() -> Option<String> {
    resolve_container_socket_with(
        std::env::var("DOCKER_HOST").ok().as_deref(),
        DOCKER_ROOTFUL_SOCKET,
        PODMAN_ROOTFUL_SOCKET,
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
    )
}

fn resolve_container_socket_with(
    docker_host: Option<&str>,
    docker_rootful: &str,
    podman_rootful: &str,
    xdg_runtime_dir: Option<&str>,
) -> Option<String> {
    if let Some(host) = docker_host
        && let Some(path) = host.strip_prefix("unix://")
        && std::path::Path::new(path).exists()
    {
        return Some(path.to_string());
    }

    if std::path::Path::new(docker_rootful).exists() {
        return Some(docker_rootful.to_string());
    }

    if std::path::Path::new(podman_rootful).exists() {
        return Some(podman_rootful.to_string());
    }

    if let Some(runtime_dir) = xdg_runtime_dir {
        let rootless_socket = format!("{runtime_dir}/podman/podman.sock");
        if std::path::Path::new(&rootless_socket).exists() {
            return Some(rootless_socket);
        }
    }

    None
}

fn connect_container_runtime(socket_path: &str) -> anyhow::Result<bollard::Docker> {
    let client_version = bollard::ClientVersion {
        major_version: 1,
        minor_version: 40,
    };

    bollard::Docker::connect_with_socket(socket_path, 120, &client_version).map_err(|error| {
        anyhow::anyhow!("failed to connect to container runtime at {socket_path}: {error}")
    })
}

#[derive(Debug, Clone)]
struct ApplyCapability {
    can_apply: bool,
    cannot_apply_reason: Option<String>,
    docker_image: Option<String>,
    socket_path: Option<String>,
}

async fn detect_apply_capability(deployment: Deployment) -> ApplyCapability {
    match deployment {
        Deployment::Native => ApplyCapability {
            can_apply: false,
            cannot_apply_reason: Some(
                "Native/source installs update manually (rebuild + restart).".to_string(),
            ),
            docker_image: None,
            socket_path: None,
        },
        Deployment::Hosted => ApplyCapability {
            can_apply: false,
            cannot_apply_reason: Some(
                "Hosted instances are updated by platform rollout, not self-service.".to_string(),
            ),
            docker_image: None,
            socket_path: None,
        },
        Deployment::Docker => {
            let Some(socket_path) = resolve_container_socket() else {
                return ApplyCapability {
                    can_apply: false,
                    cannot_apply_reason: Some(container_runtime_socket_hint()),
                    docker_image: None,
                    socket_path: None,
                };
            };

            let docker = match connect_container_runtime(&socket_path) {
                Ok(client) => client,
                Err(error) => {
                    return ApplyCapability {
                        can_apply: false,
                        cannot_apply_reason: Some(format!(
                            "Container runtime socket is present but cannot be opened: {error}"
                        )),
                        docker_image: None,
                        socket_path: Some(socket_path),
                    };
                }
            };

            if let Err(error) = docker.ping().await {
                return ApplyCapability {
                    can_apply: false,
                    cannot_apply_reason: Some(format!(
                        "Container runtime socket is mounted but engine is not reachable: {error}"
                    )),
                    docker_image: None,
                    socket_path: Some(socket_path),
                };
            }

            let docker_image = detect_current_docker_image(&docker).await.ok();

            ApplyCapability {
                can_apply: true,
                cannot_apply_reason: None,
                docker_image,
                socket_path: Some(socket_path),
            }
        }
    }
}

async fn detect_current_docker_image(docker: &bollard::Docker) -> anyhow::Result<String> {
    let container_id = get_own_container_id()?;
    let container_info = docker
        .inspect_container(&container_id, None)
        .await
        .map_err(|error| anyhow::anyhow!("failed to inspect container: {error}"))?;

    let image = container_info
        .config
        .as_ref()
        .and_then(|config| config.image.as_deref())
        .ok_or_else(|| anyhow::anyhow!("could not determine current image"))?;

    Ok(image.to_string())
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
    let capability = detect_apply_capability(current.deployment).await;
    if !capability.can_apply {
        anyhow::bail!(
            "{}",
            capability
                .cannot_apply_reason
                .unwrap_or_else(|| "Container runtime socket not available".to_string())
        );
    }

    let socket_path = capability
        .socket_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("container runtime socket path not available"))?;

    let latest_version = current
        .latest_version
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no latest version"))?;

    tracing::info!(
        from = CURRENT_VERSION,
        to = latest_version,
        "applying Docker update"
    );

    let docker = connect_container_runtime(socket_path)?;

    // Determine which image tag this container is running
    let container_id = get_own_container_id()?;
    let container_info = docker
        .inspect_container(&container_id, None)
        .await
        .map_err(|error| anyhow::anyhow!("failed to inspect container: {}", error))?;

    let current_image = container_info
        .config
        .as_ref()
        .and_then(|c| c.image.as_deref())
        .ok_or_else(|| anyhow::anyhow!("could not determine current image"))?
        .to_string();

    // Resolve the target image: same base name, new version tag.
    // e.g. ghcr.io/spacedriveapp/spacebot:v0.1.0-slim -> ghcr.io/spacedriveapp/spacebot:v0.2.0-slim
    let runtime_variant = detect_runtime_image_variant();
    let target_image = resolve_target_image(&current_image, latest_version, runtime_variant);

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
                let error_text = error.to_string();
                if error_text.contains("manifest unknown")
                    || error_text.contains("not found")
                    || error_text.contains("pull access denied")
                {
                    anyhow::bail!(
                        "image pull failed for {target_image}. this image does not have Spacebot release tags; rebuild and redeploy manually"
                    );
                }
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
        .map_err(|error| anyhow::anyhow!("failed to create new container: {}", error))?;

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
        .map_err(|error| anyhow::anyhow!("failed to rename old container: {}", error))?;

    // Rename new container to the original name
    docker
        .rename_container(
            &new_container.id,
            bollard::container::RenameContainerOptions {
                name: &container_name,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to rename new container: {}", error))?;

    // Start the new container
    docker
        .start_container::<String>(&new_container.id, None)
        .await
        .map_err(|error| anyhow::anyhow!("failed to start new container: {}", error))?;

    tracing::info!("new container started, stopping old container");

    // Stop the old container (ourselves). This process will terminate.
    docker
        .stop_container(
            &container_id,
            Some(bollard::container::StopContainerOptions { t: 10 }),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to stop old container: {}", error))?;

    // Remove the old container after stop
    let remove_result = docker
        .remove_container(
            &container_id,
            Some(bollard::container::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    if let Err(error) = remove_result {
        tracing::warn!(
            container_id = %container_id,
            %error,
            "best-effort removal of old container failed during shutdown"
        );
    }

    // We shouldn't reach here since stop_container kills us,
    // but just in case:
    std::process::exit(0);
}

/// Read this container's ID from /proc/self/mountinfo or the hostname.
fn get_own_container_id() -> anyhow::Result<String> {
    // /proc/self/mountinfo includes full IDs for both Docker and Podman.
    if let Ok(content) = std::fs::read_to_string("/proc/self/mountinfo") {
        if let Some(id) = parse_container_id_from_mountinfo(&content) {
            return Ok(id);
        }
    }

    // Docker commonly sets hostname to the short container ID.
    if let Ok(hostname) = std::fs::read_to_string("/etc/hostname") {
        let hostname = hostname.trim();
        if hostname.len() >= 12 && hostname.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(hostname.to_string());
        }
    }

    anyhow::bail!("could not determine own container ID")
}

fn parse_container_id_from_mountinfo(content: &str) -> Option<String> {
    for marker in ["/docker/containers/", "/overlay-containers/"] {
        for line in content.lines() {
            if let Some(position) = line.find(marker) {
                let after_marker = &line[position + marker.len()..];
                if let Some(end_index) = after_marker.find('/') {
                    let id = &after_marker[..end_index];
                    if id.len() == 64 && id.chars().all(|character| character.is_ascii_hexdigit()) {
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
///   - `ghcr.io/spacedriveapp/spacebot:latest` + `0.2.0` + full runtime -> `ghcr.io/spacedriveapp/spacebot:v0.2.0-full`
fn resolve_target_image(
    current_image: &str,
    new_version: &str,
    runtime_variant: Option<ImageVariant>,
) -> String {
    let image_without_digest = current_image
        .split_once('@')
        .map(|(name, _)| name)
        .unwrap_or(current_image);

    let last_slash = image_without_digest.rfind('/');
    let last_colon = image_without_digest.rfind(':');

    let (base, tag) = match last_colon {
        Some(colon) if last_slash.is_none_or(|slash| colon > slash) => (
            &image_without_digest[..colon],
            &image_without_digest[colon + 1..],
        ),
        _ => (image_without_digest, "latest"),
    };

    let variant = detect_variant_from_tag(tag)
        .or(runtime_variant)
        .unwrap_or(ImageVariant::Slim);

    format!("{}:v{}-{}", base, new_version, variant.as_str())
}

fn detect_variant_from_tag(tag: &str) -> Option<ImageVariant> {
    if tag.contains("full") {
        Some(ImageVariant::Full)
    } else if tag.contains("slim") {
        Some(ImageVariant::Slim)
    } else {
        None
    }
}

fn detect_runtime_image_variant() -> Option<ImageVariant> {
    if let Ok(chrome_path) = std::env::var("CHROME_PATH")
        && !chrome_path.is_empty()
        && std::path::Path::new(&chrome_path).exists()
    {
        return Some(ImageVariant::Full);
    }

    if std::path::Path::new("/usr/bin/chromium").exists()
        || std::path::Path::new("/usr/bin/chromium-browser").exists()
    {
        return Some(ImageVariant::Full);
    }

    None
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
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot:v0.1.0-slim",
                "0.2.0",
                Some(ImageVariant::Full)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-slim"
        );
        assert_eq!(
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot:v0.1.0-full",
                "0.2.0",
                Some(ImageVariant::Slim)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-full"
        );
        assert_eq!(
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot:latest",
                "0.2.0",
                Some(ImageVariant::Full)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-full"
        );
        assert_eq!(
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot:latest",
                "0.2.0",
                Some(ImageVariant::Slim)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-slim"
        );
        assert_eq!(
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot:slim",
                "0.2.0",
                Some(ImageVariant::Full)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-slim"
        );
        assert_eq!(
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot:v0.1.0",
                "0.2.0",
                Some(ImageVariant::Full)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-full"
        );
        assert_eq!(
            resolve_target_image(
                "registry.local:5000/spacebot",
                "0.2.0",
                Some(ImageVariant::Full)
            ),
            "registry.local:5000/spacebot:v0.2.0-full"
        );
        assert_eq!(
            resolve_target_image(
                "ghcr.io/spacedriveapp/spacebot@sha256:abcdef",
                "0.2.0",
                Some(ImageVariant::Full)
            ),
            "ghcr.io/spacedriveapp/spacebot:v0.2.0-full"
        );
    }

    fn create_fake_socket(temp_dir: &tempfile::TempDir, name: &str) -> String {
        let path = temp_dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories for fake socket");
        }
        std::fs::File::create(&path).expect("create fake socket file");
        path.to_string_lossy().to_string()
    }

    #[test]
    fn test_resolve_container_socket_with_docker_rootful() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let docker_socket = create_fake_socket(&temp_dir, "docker.sock");

        let resolved =
            resolve_container_socket_with(None, &docker_socket, "/tmp/missing-podman.sock", None);
        assert_eq!(resolved.as_deref(), Some(docker_socket.as_str()));
    }

    #[test]
    fn test_resolve_container_socket_with_podman_rootful() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let podman_socket = create_fake_socket(&temp_dir, "podman.sock");

        let resolved =
            resolve_container_socket_with(None, "/tmp/missing-docker.sock", &podman_socket, None);
        assert_eq!(resolved.as_deref(), Some(podman_socket.as_str()));
    }

    #[test]
    fn test_resolve_container_socket_with_podman_rootless() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let runtime_dir = temp_dir.path().to_string_lossy().to_string();
        let podman_socket = create_fake_socket(&temp_dir, "podman/podman.sock");

        let resolved = resolve_container_socket_with(
            None,
            "/tmp/missing-docker.sock",
            "/tmp/missing-podman.sock",
            Some(&runtime_dir),
        );
        assert_eq!(resolved, Some(podman_socket));
    }

    #[test]
    fn test_resolve_container_socket_with_docker_host_unix() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let custom_socket = create_fake_socket(&temp_dir, "custom.sock");
        let docker_host = format!("unix://{custom_socket}");

        let resolved = resolve_container_socket_with(
            Some(&docker_host),
            "/tmp/missing-docker.sock",
            "/tmp/missing-podman.sock",
            None,
        );
        assert_eq!(resolved.as_deref(), Some(custom_socket.as_str()));
    }

    #[test]
    fn test_resolve_container_socket_with_tcp_docker_host_falls_back() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let docker_socket = create_fake_socket(&temp_dir, "docker.sock");

        let resolved = resolve_container_socket_with(
            Some("tcp://127.0.0.1:2376"),
            &docker_socket,
            "/tmp/missing-podman.sock",
            None,
        );
        assert_eq!(resolved.as_deref(), Some(docker_socket.as_str()));
    }

    #[test]
    fn test_resolve_container_socket_with_none() {
        let resolved = resolve_container_socket_with(
            None,
            "/tmp/missing-docker.sock",
            "/tmp/missing-podman.sock",
            None,
        );
        assert_eq!(resolved, None);
    }

    const DOCKER_ID: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    const PODMAN_ID: &str = "0011223344556677889900112233445566778899001122334455667788990011";

    #[test]
    fn test_parse_container_id_from_mountinfo_docker() {
        let mountinfo = format!(
            "123 0 0:1 / / rw - ext4 /dev/sda rw\n456 123 0:2 / /var/lib/docker/containers/{DOCKER_ID}/shm rw\n"
        );

        assert_eq!(
            parse_container_id_from_mountinfo(&mountinfo).as_deref(),
            Some(DOCKER_ID)
        );
    }

    #[test]
    fn test_parse_container_id_from_mountinfo_podman() {
        let mountinfo = format!(
            "123 0 0:1 / / rw - ext4 /dev/sda rw\n456 123 0:2 / /var/lib/containers/storage/overlay-containers/{PODMAN_ID}/userdata rw\n"
        );

        assert_eq!(
            parse_container_id_from_mountinfo(&mountinfo).as_deref(),
            Some(PODMAN_ID)
        );
    }

    #[test]
    fn test_parse_container_id_from_mountinfo_missing() {
        let mountinfo = "123 0 0:1 / / rw - ext4 /dev/sda rw\n";
        assert_eq!(parse_container_id_from_mountinfo(mountinfo), None);
    }

    #[test]
    fn test_parse_container_id_from_mountinfo_short_id_ignored() {
        let mountinfo = "456 123 0:2 / /docker/containers/abc123/shm rw\n";
        assert_eq!(parse_container_id_from_mountinfo(mountinfo), None);
    }
}
