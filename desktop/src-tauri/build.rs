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

    // In dev mode, copy the sidecar binary next to the desktop executable.
    // Tauri resolves sidecars as `{exe_dir}/{name}.exe` but the sidecar is built
    // separately. Without this, the sidecar is not found at runtime.
    #[cfg(windows)]
    {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
        let out_path = std::path::Path::new(&out_dir);
        // OUT_DIR is like <target>/debug/build/<crate>/out — walk up to <target>/debug
        if let Some(target_debug) = out_path.ancestors().nth(3) {
            let dest = target_debug.join("spacebot.exe");
            let manifest_dir =
                std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
            let triple = std::env::var("TAURI_ENV_TARGET_TRIPLE")
                .or_else(|_| std::env::var("TARGET"))
                .unwrap_or_else(|_| "x86_64-pc-windows-msvc".to_string());
            let src =
                std::path::Path::new(&manifest_dir).join(format!("binaries/spacebot-{triple}.exe"));
            if !src.exists() {
                panic!(
                    "sidecar binary not found at {}. Build the server first \
                     (`cargo build -p spacebot`) then copy it to binaries/.",
                    src.display()
                );
            }

            let needs_copy = !dest.exists()
                || src.metadata().and_then(|s| s.modified()).ok()
                    > dest.metadata().and_then(|d| d.modified()).ok();
            if needs_copy {
                println!("cargo:warning=Copying sidecar to {}", dest.display());
                std::fs::copy(&src, &dest).unwrap_or_else(|error| {
                    panic!(
                        "failed to copy sidecar from {} to {}: {error}",
                        src.display(),
                        dest.display()
                    )
                });
            }
            println!("cargo:rerun-if-changed={}", src.display());
        }
    }

    tauri_build::build()
}
