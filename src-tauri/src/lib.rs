mod mixer;

use std::sync::Arc;
use std::time::Duration;

use mixer::{AppSession, AudioMixerBackend, MixerError};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};

struct MixerState {
    backend: Arc<dyn AudioMixerBackend>,
}

fn mixer_error_to_string(err: MixerError) -> String {
    err.to_string()
}

#[tauri::command]
fn list_sessions(state: State<MixerState>) -> Result<Vec<AppSession>, String> {
    state.backend.list_sessions().map_err(mixer_error_to_string)
}

#[tauri::command]
fn set_volume(state: State<MixerState>, session_id: String, volume: f32) -> Result<(), String> {
    state
        .backend
        .set_volume(&session_id, volume)
        .map_err(mixer_error_to_string)
}

#[tauri::command]
fn set_muted(state: State<MixerState>, session_id: String, muted: bool) -> Result<(), String> {
    state
        .backend
        .set_muted(&session_id, muted)
        .map_err(mixer_error_to_string)
}

/// How often we re-poll the platform backend for session changes. WASAPI/PulseAudio don't give
/// us a cheap cross-platform push notification in v1, so we poll and only emit to the frontend
/// when the list actually differs from what we last sent (see `mixer::AppSession`'s `PartialEq`).
const POLL_INTERVAL: Duration = Duration::from_millis(700);

fn spawn_session_poll_loop(app_handle: AppHandle, backend: Arc<dyn AudioMixerBackend>) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<Vec<AppSession>> = None;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            match backend.list_sessions() {
                Ok(sessions) => {
                    if last.as_ref() != Some(&sessions) {
                        let _ = app_handle.emit("sessions-changed", &sessions);
                        last = Some(sessions);
                    }
                }
                Err(err) => {
                    log::warn!("failed to list audio sessions: {err}");
                }
            }
        }
    });
}

/// Unique id for the (single) tray icon, so the "Show Mixolume" menu item can look it up via
/// [`tauri::Manager::tray_by_id`] and read its current on-screen position -- the menu item click
/// handler only gets an `AppHandle`, not the `&TrayIcon` a direct tray-icon click gives for free.
const TRAY_ICON_ID: &str = "mixolume-tray";

/// Moves the main window so it appears directly under the tray icon, like a native menu-bar
/// app (Control Center, Wi-Fi/Bluetooth menu extras, etc.) rather than wherever the window
/// manager last happened to place it.
///
/// `tray_rect`'s `position`/`size` are DPI-aware (`tauri::Position`/`Size`, not raw physical
/// pixels) -- converted via the window's own `scale_factor()` before arithmetic, since mixing a
/// logical tray-icon rect with a physical window size would misplace the window on any non-1x
/// display.
fn position_window_under_tray(window: &tauri::WebviewWindow, tray_rect: tauri::Rect) {
    let Ok(scale) = window.scale_factor() else {
        return;
    };
    let Ok(window_size) = window.outer_size() else {
        return;
    };
    let icon_pos = tray_rect.position.to_physical::<f64>(scale);
    let icon_size = tray_rect.size.to_physical::<f64>(scale);

    let x = icon_pos.x + (icon_size.width / 2.0) - (window_size.width as f64 / 2.0);
    // A few points of gap below the icon, same idea as a native menu-bar dropdown.
    let y = icon_pos.y + icon_size.height + 4.0;
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

fn show_main_window_near_tray(app: &AppHandle, tray_rect: Option<tauri::Rect>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let rect = tray_rect.or_else(|| {
        app.tray_by_id(TRAY_ICON_ID)
            .and_then(|tray| tray.rect().ok().flatten())
    });
    if let Some(rect) = rect {
        position_window_under_tray(&window, rect);
    }
    let _ = window.show();
    let _ = window.set_focus();
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show Mixolume", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "quit" => app.exit(0),
            "show" => show_main_window_near_tray(app, None),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                show_main_window_near_tray(tray.app_handle(), Some(rect));
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Auto-hide on focus loss, like a native menu-bar popover (Control Center, Wi-Fi/
        // Bluetooth menu extras): clicking anywhere outside the window closes it instead of
        // leaving it stranded on screen behind whatever the user clicked into next.
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            // Menu-bar-only, like Control Center / Bluetooth / Wi-Fi menu extras -- no Dock icon,
            // no Cmd+Tab entry. `skipTaskbar` in tauri.conf.json only hides the *window* from
            // window-switcher-style UI; the Dock icon specifically is controlled by the app's
            // activation policy, which defaults to `Regular` (a normal Dock-visible app) unless
            // set otherwise here.
            #[cfg(target_os = "macos")]
            app.handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory)?;

            let backend: Arc<dyn AudioMixerBackend> = Arc::from(mixer::new_platform_backend());
            app.manage(MixerState {
                backend: backend.clone(),
            });
            spawn_session_poll_loop(app.handle().clone(), backend);
            setup_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            set_volume,
            set_muted
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
