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

fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show Mixolume", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "quit" => app.exit(0),
            "show" => toggle_main_window(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
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
        .setup(|app| {
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
