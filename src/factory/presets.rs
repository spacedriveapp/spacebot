//! Preset archetypes embedded in the binary via rust-embed.
//!
//! Each preset is a directory under `presets/` containing:
//! - `meta.toml` — id, name, description, icon, tags, default config
//! - `SOUL.md` — personality, voice, values, boundaries
//! - `IDENTITY.md` — what the agent is, what it does, scope
//! - `ROLE.md` — behavioral rules, delegation, escalation
//! - `SPEECH.md` — spoken response style and anti-repetition guidance

use rust_embed::Embed;

/// Embedded preset files from the `presets/` directory at the repo root.
#[derive(Embed)]
#[folder = "presets/"]
struct PresetAssets;

/// Metadata for a preset archetype (returned in list responses).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PresetMeta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub defaults: PresetDefaults,
}

/// Default operational parameters suggested by a preset.
///
/// Model routing is intentionally excluded — presets are provider-agnostic.
/// The factory conversation handles model selection at creation time when the
/// user's available providers are known.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PresetDefaults {
    pub max_concurrent_workers: Option<u32>,
    pub max_turns: Option<u32>,
}

/// A fully loaded preset with all identity file content.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Preset {
    pub meta: PresetMeta,
    pub soul: String,
    pub identity: String,
    pub role: String,
    pub speech: Option<String>,
}

/// Registry for accessing embedded preset archetypes.
pub struct PresetRegistry;

impl PresetRegistry {
    /// List metadata for all available presets.
    pub fn list() -> Vec<PresetMeta> {
        let mut presets = Vec::new();

        // Discover preset directories by finding all meta.toml files
        for path in PresetAssets::iter() {
            let path_str = path.as_ref();
            if path_str.ends_with("/meta.toml")
                && let Some(meta) = Self::load_meta(path_str)
            {
                presets.push(meta);
            }
        }

        presets.sort_by(|a, b| a.name.cmp(&b.name));
        presets
    }

    /// Load a full preset by ID, including all identity file content.
    pub fn load(id: &str) -> Option<Preset> {
        let meta = Self::load_meta(&format!("{id}/meta.toml"))?;
        let soul = Self::load_file(id, "SOUL.md")?;
        let identity = Self::load_file(id, "IDENTITY.md")?;
        let role = Self::load_file(id, "ROLE.md")?;
        let speech = Self::load_optional_file(id, "SPEECH.md");

        Some(Preset {
            meta,
            soul,
            identity,
            role,
            speech,
        })
    }

    /// Load and parse a meta.toml file from embedded assets.
    fn load_meta(path: &str) -> Option<PresetMeta> {
        let file = PresetAssets::get(path)?;
        let content = std::str::from_utf8(file.data.as_ref()).ok()?;
        let meta: PresetMeta = toml::from_str(content)
            .map_err(|error| {
                tracing::warn!(%error, path, "failed to parse preset meta.toml");
                error
            })
            .ok()?;
        Some(meta)
    }

    /// Load a single file from a preset directory.
    fn load_file(preset_id: &str, filename: &str) -> Option<String> {
        let path = format!("{preset_id}/{filename}");
        let file = PresetAssets::get(&path)?;
        let content = std::str::from_utf8(file.data.as_ref()).ok()?;
        Some(content.to_string())
    }

    fn load_optional_file(preset_id: &str, filename: &str) -> Option<String> {
        let path = format!("{preset_id}/{filename}");
        let file = PresetAssets::get(&path)?;
        let content = std::str::from_utf8(file.data.as_ref()).ok()?;
        Some(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_all_presets() {
        let presets = PresetRegistry::list();
        assert_eq!(
            presets.len(),
            9,
            "expected 9 presets, got {}",
            presets.len()
        );

        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"main-agent"));
        assert!(ids.contains(&"community-manager"));
        assert!(ids.contains(&"research-analyst"));
        assert!(ids.contains(&"customer-support"));
        assert!(ids.contains(&"engineering-assistant"));
        assert!(ids.contains(&"content-writer"));
        assert!(ids.contains(&"sales-bdr"));
        assert!(ids.contains(&"executive-assistant"));
        assert!(ids.contains(&"project-manager"));
    }

    #[test]
    fn load_returns_full_preset() {
        let preset = PresetRegistry::load("community-manager")
            .expect("community-manager preset should exist");

        assert_eq!(preset.meta.id, "community-manager");
        assert_eq!(preset.meta.name, "Community Manager");
        assert!(!preset.meta.description.is_empty());
        assert!(!preset.meta.tags.is_empty());
        assert!(!preset.soul.is_empty());
        assert!(!preset.identity.is_empty());
        assert!(!preset.role.is_empty());
    }

    #[test]
    fn load_nonexistent_returns_none() {
        assert!(PresetRegistry::load("nonexistent").is_none());
    }

    #[test]
    fn all_presets_load_successfully() {
        for meta in PresetRegistry::list() {
            let preset = PresetRegistry::load(&meta.id)
                .unwrap_or_else(|| panic!("preset '{}' should load fully", meta.id));
            assert!(!preset.soul.is_empty(), "{}: SOUL.md is empty", meta.id);
            assert!(
                !preset.identity.is_empty(),
                "{}: IDENTITY.md is empty",
                meta.id
            );
            assert!(!preset.role.is_empty(), "{}: ROLE.md is empty", meta.id);
        }
    }

    #[test]
    fn meta_has_required_fields() {
        for meta in PresetRegistry::list() {
            assert!(!meta.id.is_empty(), "preset id is empty");
            assert!(!meta.name.is_empty(), "{}: name is empty", meta.id);
            assert!(
                !meta.description.is_empty(),
                "{}: description is empty",
                meta.id
            );
            assert!(!meta.icon.is_empty(), "{}: icon is empty", meta.id);
        }
    }
}
