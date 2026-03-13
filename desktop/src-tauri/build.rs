#[cfg(target_os = "macos")]
use std::process::Command;

fn main() {
    // Compile .icon to Assets.car on macOS (liquid glass icon support)
    #[cfg(target_os = "macos")]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
        let icon_source = format!("{}/../assets/Spacebot.icon", manifest_dir);
        let gen_dir = format!("{}/gen/icon", manifest_dir);

        std::fs::create_dir_all(&gen_dir).expect("Failed to create gen/icon directory");

        if std::path::Path::new(&icon_source).exists() {
            println!("cargo:rerun-if-changed={}", icon_source);

            let output = Command::new("xcrun")
                .args([
                    "actool",
                    &icon_source,
                    "--compile",
                    &gen_dir,
                    "--output-format",
                    "human-readable-text",
                    "--notices",
                    "--warnings",
                    "--errors",
                    "--output-partial-info-plist",
                    &format!("{}/partial.plist", gen_dir),
                    "--app-icon",
                    "Spacebot",
                    "--include-all-app-icons",
                    "--enable-on-demand-resources",
                    "NO",
                    "--development-region",
                    "en",
                    "--target-device",
                    "mac",
                    "--minimum-deployment-target",
                    "11.0",
                    "--platform",
                    "macosx",
                ])
                .output()
                .expect("Failed to execute actool");

            if !output.status.success() {
                eprintln!("actool stderr: {}", String::from_utf8_lossy(&output.stderr));
                eprintln!("actool stdout: {}", String::from_utf8_lossy(&output.stdout));
                println!("cargo:warning=actool failed to compile Spacebot.icon");
            } else {
                println!("cargo:warning=Compiled Spacebot.icon to Assets.car");
            }
        } else {
            println!("cargo:warning=Spacebot.icon not found at {}", icon_source);
        }
    }

    tauri_build::build()
}
