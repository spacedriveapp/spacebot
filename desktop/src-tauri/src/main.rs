// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Apply macOS titlebar style (invisible toolbar for traffic light padding)
            #[cfg(target_os = "macos")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    match window.ns_window() {
                        Ok(ns_window) => unsafe {
                            sb_desktop_macos::set_titlebar_style(&ns_window, false);
                            sb_desktop_macos::lock_app_theme(1); // Dark theme
                        },
                        Err(e) => {
                            tracing::warn!("Could not get NSWindow handle: {}", e);
                        }
                    }
                }
            }

            // Show window after setup
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Re-apply titlebar style on fullscreen transitions (macOS)
            #[cfg(target_os = "macos")]
            if let tauri::WindowEvent::Resized(_) = event {
                if let Ok(is_fullscreen) = window.is_fullscreen() {
                    if let Ok(ns_window) = window.ns_window() {
                        unsafe {
                            sb_desktop_macos::set_titlebar_style(&ns_window, is_fullscreen);
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error running Spacebot");
}
