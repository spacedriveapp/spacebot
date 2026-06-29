//! Generates a sideloadable Microsoft Teams app package (manifest + icons, zipped)
//! so operators don't have to hand-write the manifest. The manifest gotchas
//! (schema 1.17, distinct app id, lowercase scopes, no packageName, required
//! accentColor/validDomains) are baked in; default icons are the Spacebot logo.

use std::io::{Cursor, Write as _};
use std::path::Path;

use image::imageops::FilterType;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

/// The Spacebot logo, bundled at compile time. 384x384 RGBA PNG.
const SPACEBOT_LOGO: &[u8] = include_bytes!("../../interface/public/ball.png");

/// Build the Teams app manifest. `app_id` is the bot's App (client) ID
/// (`bots[].botId`); `manifest_id` is the distinct Teams app id (`id`).
pub fn teams_manifest_json(app_id: &str, manifest_id: &str) -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://developer.microsoft.com/en-us/json-schemas/teams/v1.17/MicrosoftTeams.schema.json",
        "manifestVersion": "1.17",
        "version": "1.0.0",
        "id": manifest_id,
        "developer": {
            "name": "Spacebot",
            "websiteUrl": "https://github.com/spacedriveapp/spacebot",
            "privacyUrl": "https://github.com/spacedriveapp/spacebot",
            "termsOfUseUrl": "https://github.com/spacedriveapp/spacebot"
        },
        "icons": { "color": "icon-color.png", "outline": "icon-outline.png" },
        "name": { "short": "Spacebot", "full": "Spacebot — AI assistant" },
        "description": {
            "short": "AI assistant powered by Spacebot.",
            "full": "Chat with your Spacebot agent in Microsoft Teams."
        },
        "accentColor": "#6264A7",
        "bots": [{
            "botId": app_id,
            "scopes": ["personal", "team", "groupchat"],
            "supportsFiles": false,
            "isNotificationOnly": false
        }],
        "validDomains": []
    })
}
// NOTE: do NOT add a `permissions` key. The manifest validated against real
// Teams (docs/design-docs/teams-setup.md) has no `permissions` field; schema
// 1.17 rejects undefined/extra properties, and `permissions` is deprecated.

/// Render the two Teams icons from the bundled logo: color 192x192 (the logo
/// resized) and outline 32x32 (a white silhouette on transparent, per Teams'
/// outline-icon requirement).
pub fn render_default_icons() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let logo = image::load_from_memory(SPACEBOT_LOGO)?;

    let mut color = Vec::new();
    logo.resize_exact(192, 192, FilterType::Lanczos3)
        .write_to(&mut Cursor::new(&mut color), image::ImageFormat::Png)?;

    // Outline: resize to 32x32, then make every non-transparent pixel white
    // (Teams renders the outline icon monochrome on a transparent background).
    let mut outline_img = logo.resize_exact(32, 32, FilterType::Lanczos3).to_rgba8();
    for px in outline_img.pixels_mut() {
        let alpha = px.0[3];
        px.0 = [255, 255, 255, alpha];
    }
    let mut outline = Vec::new();
    image::DynamicImage::ImageRgba8(outline_img)
        .write_to(&mut Cursor::new(&mut outline), image::ImageFormat::Png)?;

    Ok((color, outline))
}

/// Build the `.zip` package: manifest.json + the two icons at the archive root.
pub fn build_app_package(app_id: &str, manifest_id: &str) -> anyhow::Result<Vec<u8>> {
    let manifest = serde_json::to_vec_pretty(&teams_manifest_json(app_id, manifest_id))?;
    let (color, outline) = render_default_icons()?;

    let mut cursor = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut cursor);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    for (name, bytes) in [
        ("manifest.json", manifest.as_slice()),
        ("icon-color.png", color.as_slice()),
        ("icon-outline.png", outline.as_slice()),
    ] {
        zip.start_file(name, opts)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(cursor.into_inner())
}

/// Read the persisted Teams app `id`, or generate+persist a fresh GUID. Keeping
/// it stable means re-downloading produces the same `id`, so re-uploading the
/// package updates the existing Teams app instead of creating a duplicate.
pub fn load_or_create_manifest_id(instance_dir: &Path) -> String {
    let path = instance_dir.join("teams_manifest_id.json");
    if let Ok(contents) = std::fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents)
        && let Some(id) = v.get("manifest_id").and_then(|x| x.as_str())
        && !id.is_empty()
    {
        return id.to_string();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::json!({ "manifest_id": id }).to_string();
    if std::fs::write(&tmp, &json).is_ok() {
        if let Err(e) = std::fs::rename(&tmp, &path) {
            tracing::warn!(%e, ?path, "teams manifest-id sidecar: rename failed");
        }
    } else {
        tracing::warn!(?path, "teams manifest-id sidecar: write failed");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    #[test]
    fn manifest_has_required_fields_and_no_packagename() {
        let m = teams_manifest_json(
            "00000000-0000-0000-0000-000000000001",
            "ffffffff-0000-0000-0000-000000000002",
        );
        assert_eq!(m["manifestVersion"], "1.17");
        assert_eq!(m["id"], "ffffffff-0000-0000-0000-000000000002");
        // id MUST differ from botId.
        assert_ne!(m["id"], m["bots"][0]["botId"]);
        assert_eq!(
            m["bots"][0]["botId"],
            "00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(m["bots"][0]["scopes"][0], "personal");
        assert_eq!(m["bots"][0]["scopes"][1], "team");
        assert_eq!(m["bots"][0]["scopes"][2], "groupchat");
        assert!(m.get("accentColor").is_some());
        assert!(m.get("validDomains").is_some());
        assert!(
            m.get("packageName").is_none(),
            "schema 1.17 rejects packageName"
        );
        assert_eq!(m["icons"]["color"], "icon-color.png");
        assert_eq!(m["icons"]["outline"], "icon-outline.png");
    }

    #[test]
    fn icons_render_at_required_dimensions() {
        let (color, outline) = render_default_icons().expect("icons render");
        let c = image::load_from_memory(&color).expect("color png");
        assert_eq!((c.width(), c.height()), (192, 192));
        let o = image::load_from_memory(&outline).expect("outline png");
        assert_eq!((o.width(), o.height()), (32, 32));
    }

    #[test]
    fn package_zip_contains_the_three_root_files() {
        let bytes = build_app_package(
            "00000000-0000-0000-0000-000000000001",
            "ffffffff-0000-0000-0000-000000000002",
        )
        .expect("package builds");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip opens");
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"manifest.json".to_string()));
        assert!(names.contains(&"icon-color.png".to_string()));
        assert!(names.contains(&"icon-outline.png".to_string()));
        // manifest.json parses and round-trips the ids.
        let mut mf = zip.by_name("manifest.json").unwrap();
        let mut s = String::new();
        mf.read_to_string(&mut s).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            parsed["bots"][0]["botId"],
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn manifest_id_sidecar_is_stable() {
        let dir = std::env::temp_dir().join(format!("teams-pkg-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = load_or_create_manifest_id(&dir);
        let b = load_or_create_manifest_id(&dir);
        assert_eq!(a, b, "second call reuses the persisted id");
        assert_eq!(a.len(), 36, "looks like a GUID");
        std::fs::remove_dir_all(&dir).ok();
    }
}
