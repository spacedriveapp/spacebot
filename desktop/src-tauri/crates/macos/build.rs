#[cfg(target_os = "macos")]
use std::env;

fn main() {
    #[cfg(target_os = "macos")]
    {
        // Tauri defaults MACOSX_DEPLOYMENT_TARGET to 10.13, which is too old for
        // the AppKit APIs we use (darkAqua requires 10.14, NSApp.appearance requires
        // 10.14). Enforce a floor of 11.0 to match Package.swift.
        let min_version = (11, 0);
        let deployment_target = env::var("MACOSX_DEPLOYMENT_TARGET")
            .ok()
            .and_then(|v| {
                let mut parts = v.split('.');
                let major = parts.next()?.parse::<u32>().ok()?;
                let minor = parts
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if (major, minor) >= min_version {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| format!("{}.{}", min_version.0, min_version.1));

        // SAFETY: build scripts are single-threaded at this point.
        unsafe { env::set_var("MACOSX_DEPLOYMENT_TARGET", &deployment_target) };

        swift_rs::SwiftLinker::new(deployment_target.as_str())
            .with_package("sb-desktop-macos", "./")
            .link();
    }
}
