// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::PathBuf;
use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// Resolve the path to the connection settings file in the app data directory.
fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("failed to resolve app data dir: {error}"))?;
    Ok(dir.join("connection.json"))
}

/// Read the saved server URL, or return the default.
#[tauri::command]
fn get_server_url(app: tauri::AppHandle) -> String {
    let Ok(path) = settings_path(&app) else {
        return "http://localhost:19898".to_string();
    };
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(url) = value.get("server_url").and_then(|v| v.as_str()) {
                return url.to_string();
            }
        }
    }
    "http://localhost:19898".to_string()
}

/// Persist the server URL to disk.
#[tauri::command]
fn set_server_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let path = settings_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let value = serde_json::json!({ "server_url": url });
    let contents = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    fs::write(&path, contents).map_err(|error| error.to_string())?;
    Ok(())
}

/// Toggle the voice overlay window visibility.
#[tauri::command]
fn toggle_voice_overlay(app: tauri::AppHandle) -> Result<(), String> {
    toggle_overlay(&app);
    Ok(())
}

fn toggle_overlay(app: &tauri::AppHandle) {
    if let Some(overlay) = app.get_webview_window("voice-overlay") {
        // Toggle visibility
        if overlay.is_visible().unwrap_or(false) {
            let _ = overlay.hide();
        } else {
            let _ = overlay.show();
            let _ = overlay.set_focus();
        }
    } else {
        // Create the overlay window on first toggle
        create_overlay_window(app);
    }
}

fn create_overlay_window(app: &tauri::AppHandle) {
    use tauri::WebviewWindowBuilder;

    // Get the primary monitor to position at bottom center
    let monitor = app.primary_monitor().ok().flatten();

    let screen_width = monitor
        .as_ref()
        .map(|m| m.size().width as f64 / m.scale_factor())
        .unwrap_or(1920.0);
    let screen_height = monitor
        .as_ref()
        .map(|m| m.size().height as f64 / m.scale_factor())
        .unwrap_or(1080.0);

    let overlay_width = 520.0;
    let overlay_height = 460.0;
    let x = (screen_width - overlay_width) / 2.0;
    let y = screen_height - overlay_height - 40.0; // 40px from bottom edge

    match WebviewWindowBuilder::new(
        app,
        "voice-overlay",
        tauri::WebviewUrl::App("/overlay".into()),
    )
    .title("Voice")
    .inner_size(overlay_width, overlay_height)
    .position(x, y)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .visible(true)
    .resizable(false)
    .skip_taskbar(true)
    .focused(true)
    .build()
    {
        Ok(window) => {
            tracing::info!("voice overlay window created");
            // Apply dark theme on macOS
            #[cfg(target_os = "macos")]
            {
                if let Ok(ns_window) = window.ns_window() {
                    unsafe {
                        sb_desktop_macos::lock_app_theme(1);
                    }
                    let _ = ns_window;
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, "failed to create voice overlay window");
        }
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Option+Space shortcut for voice overlay toggle
    let shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Space);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcut(shortcut)
                .unwrap()
                .with_handler(move |app, _shortcut, event| {
                    if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        toggle_overlay(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_server_url,
            set_server_url,
            toggle_voice_overlay,
        ])
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
