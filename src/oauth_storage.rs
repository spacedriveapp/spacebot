//! Shared JSON persistence helpers for OAuth credential files.

use anyhow::{Context as _, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

pub fn load_json_credentials<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }

    let data = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let credentials = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(credentials))
}

pub fn save_json_credentials<T: Serialize>(path: &Path, credentials: &T) -> Result<()> {
    let data = serde_json::to_string_pretty(credentials)
        .with_context(|| format!("failed to serialize {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.write_all(data.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, &data)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

pub fn json_credentials_path(instance_dir: &Path, file_name: &str) -> PathBuf {
    instance_dir.join(file_name)
}

#[cfg(test)]
mod tests {
    use super::{json_credentials_path, load_json_credentials, save_json_credentials};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct TestCredentials {
        access_token: String,
        refresh_token: String,
    }

    #[test]
    fn round_trips_json_credentials() {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let path = json_credentials_path(tempdir.path(), "oauth.json");
        let credentials = TestCredentials {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
        };

        save_json_credentials(&path, &credentials).expect("save credentials");
        let loaded = load_json_credentials::<TestCredentials>(&path)
            .expect("load credentials")
            .expect("credentials should exist");

        assert_eq!(loaded, credentials);
    }

    #[test]
    fn missing_json_credentials_returns_none() {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let path = json_credentials_path(tempdir.path(), "missing.json");
        let loaded =
            load_json_credentials::<TestCredentials>(&path).expect("missing credential load");

        assert!(loaded.is_none());
    }
}
