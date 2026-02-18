use std::process::Command;

fn main() {
    if std::env::var("SPACEBOT_SKIP_FRONTEND_BUILD").is_ok() {
        return;
    }
    // Re-run if interface source files change
    println!("cargo:rerun-if-changed=interface/src/");
    println!("cargo:rerun-if-changed=interface/index.html");
    println!("cargo:rerun-if-changed=interface/package.json");
    println!("cargo:rerun-if-changed=interface/vite.config.ts");
    println!("cargo:rerun-if-changed=interface/tailwind.config.ts");

    let interface_dir = std::path::Path::new("interface");

    // Skip if bun isn't installed or node_modules is missing (CI without frontend deps)
    if !interface_dir.join("node_modules").exists() {
        eprintln!(
            "cargo:warning=interface/node_modules not found, skipping frontend build. Run `bun install` in interface/"
        );
        ensure_dist_dir();
        return;
    }

    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(interface_dir)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "cargo:warning=frontend build exited with {s}, the binary will serve a stale or empty UI"
            );
        }
        Err(e) => {
            eprintln!(
                "cargo:warning=failed to run `bun run build`: {e}. Install bun to build the frontend."
            );
            ensure_dist_dir();
        }
    }
}

/// rust-embed requires the folder to exist even if empty.
fn ensure_dist_dir() {
    let dist = std::path::Path::new("interface/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).ok();
    }
}
