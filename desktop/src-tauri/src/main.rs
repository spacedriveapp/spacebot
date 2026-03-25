// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::PathBuf;
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

// ── Voice overlay dimensions ─────────────────────────────────────────────
const OVERLAY_INITIAL_WIDTH: f64 = 520.0;
const OVERLAY_INITIAL_HEIGHT: f64 = 100.0;
const OVERLAY_BOTTOM_MARGIN: f64 = 40.0;

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

/// Resize a named overlay window to the given logical dimensions.
/// Repositions so the window stays horizontally centred and bottom-pinned.
/// The frontend owns the layout — it measures its own content and tells us
/// the exact size it needs.
#[tauri::command]
fn resize_overlay_window(
    app: tauri::AppHandle,
    label: String,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window(&label) else {
        return Ok(());
    };

    let monitor = app.primary_monitor().ok().flatten();
    let screen_width = monitor
        .as_ref()
        .map(|m| m.size().width as f64 / m.scale_factor())
        .unwrap_or(1920.0);
    let screen_height = monitor
        .as_ref()
        .map(|m| m.size().height as f64 / m.scale_factor())
        .unwrap_or(1080.0);

    let x = (screen_width - width) / 2.0;
    let y = screen_height - height - OVERLAY_BOTTOM_MARGIN;

    use tauri::LogicalPosition;
    use tauri::LogicalSize;
    let _ = window.set_size(LogicalSize::new(width, height));
    let _ = window.set_position(LogicalPosition::new(x, y));

    Ok(())
}

fn activate_voice_overlay(app: &tauri::AppHandle) {
    if app.get_webview_window("voice-overlay").is_none() {
        create_overlay_window(app);
    } else if let Some(overlay) = app.get_webview_window("voice-overlay") {
        if !overlay.is_visible().unwrap_or(false) {
            apply_overlay_window_chrome(&overlay);
            let _ = overlay.show();
            let _ = overlay.set_focus();
        }
    }
}

fn toggle_overlay(app: &tauri::AppHandle) {
    if let Some(overlay) = app.get_webview_window("voice-overlay") {
        // Toggle visibility
        if overlay.is_visible().unwrap_or(false) {
            let _ = overlay.hide();
        } else {
            apply_overlay_window_chrome(&overlay);
            let _ = overlay.show();
            let _ = overlay.set_focus();
        }
    } else {
        // Create the overlay window on first toggle
        create_overlay_window(app);
    }
}

fn create_overlay_window(app: &tauri::AppHandle) {
    use tauri::window::Color;
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

    // Start collapsed (pill-only). The frontend measures its own content
    // and calls resize_overlay_window when the layout changes.
    let x = (screen_width - OVERLAY_INITIAL_WIDTH) / 2.0;
    let y = screen_height - OVERLAY_INITIAL_HEIGHT - OVERLAY_BOTTOM_MARGIN;

    match WebviewWindowBuilder::new(
        app,
        "voice-overlay",
        tauri::WebviewUrl::App("/overlay".into()),
    )
    .title("Voice")
    .inner_size(OVERLAY_INITIAL_WIDTH, OVERLAY_INITIAL_HEIGHT)
    .position(x, y)
    .decorations(false)
    .shadow(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .always_on_top(true)
    .visible(true)
    .resizable(false)
    .skip_taskbar(true)
    .focused(true)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .build()
    {
        Ok(window) => {
            apply_overlay_window_chrome(&window);
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

fn apply_overlay_window_chrome(window: &tauri::WebviewWindow) {
    let _ = window.set_decorations(false);
    let _ = window.set_shadow(false);
    let _ = window.set_always_on_top(true);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Option+Space toggles the overlay. Option+Shift+Space is hold-to-talk.
    let toggle_shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Space);
    let voice_shortcut = Shortcut::new(Some(Modifiers::ALT | Modifiers::SHIFT), Code::Space);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcut(toggle_shortcut.clone())
                .unwrap()
                .with_shortcut(voice_shortcut.clone())
                .unwrap()
                .with_handler(
                    move |app, _shortcut, event| match (_shortcut, event.state) {
                        (shortcut, tauri_plugin_global_shortcut::ShortcutState::Pressed)
                            if shortcut == &toggle_shortcut =>
                        {
                            toggle_overlay(app);
                        }
                        (shortcut, tauri_plugin_global_shortcut::ShortcutState::Pressed)
                            if shortcut == &voice_shortcut =>
                        {
                            activate_voice_overlay(app);
                            let _ = app.emit("voice-overlay:start-recording", ());
                        }
                        (shortcut, tauri_plugin_global_shortcut::ShortcutState::Released)
                            if shortcut == &voice_shortcut =>
                        {
                            let _ = app.emit("voice-overlay:stop-recording", ());
                        }
                        _ => {}
                    },
                )
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_server_url,
            set_server_url,
            toggle_voice_overlay,
            resize_overlay_window,
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
