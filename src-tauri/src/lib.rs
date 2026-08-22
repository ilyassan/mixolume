mod mixer;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mixer::{AppSession, AudioMixerBackend, MixerError};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State};

struct MixerState {
    backend: Arc<dyn AudioMixerBackend>,
}

/// How long after a programmatic `show()` the hide-on-blur handler should ignore a `Focused
/// (false)` event. Both the OS itself (bringing a freshly-created/reordered window forward) and
/// our own repaint nudge (a deliberate resize, see [`nudge_repaint`]) can emit a transient
/// focus-loss blip right around show time; without this guard either one can immediately hide the
/// window we just showed, which is exactly the "sometimes works, sometimes doesn't" flakiness
/// this was added to fix.
const POST_SHOW_BLUR_GUARD: Duration = Duration::from_millis(400);

#[derive(Default)]
struct WindowShowState {
    /// Set every time [`show_main_window_near_tray`] actually shows the window; read by the
    /// hide-on-blur handler to ignore spurious blur events shortly after.
    last_shown_at: Mutex<Option<Instant>>,
    /// The last successfully tray-anchored physical position. `TrayIconEvent::Click`'s `rect`
    /// (or the tray-by-id lookup the menu item uses) occasionally comes back `None` on a given
    /// click even when the user did click the real tray icon -- when that happens we reuse this
    /// instead of leaving the window at whatever position it last happened to have (which, for a
    /// window that's never been successfully positioned yet, is `tauri.conf.json`'s unset
    /// default, nowhere near the tray).
    last_tray_position: Mutex<Option<PhysicalPosition<f64>>>,
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
/// manager last happened to place it. Returns the computed position so the caller can cache it
/// as the fallback for the next click, in case that one can't get a tray rect at all.
///
/// `tray_rect`'s `position`/`size` are DPI-aware (`tauri::Position`/`Size`, not raw physical
/// pixels) -- converted via the window's own `scale_factor()` before arithmetic, since mixing a
/// logical tray-icon rect with a physical window size would misplace the window on any non-1x
/// display.
fn position_window_under_tray(
    window: &tauri::WebviewWindow,
    tray_rect: tauri::Rect,
) -> Option<PhysicalPosition<f64>> {
    let scale = window.scale_factor().ok()?;
    let window_size = window.outer_size().ok()?;
    let icon_pos = tray_rect.position.to_physical::<f64>(scale);
    let icon_size = tray_rect.size.to_physical::<f64>(scale);

    let x = icon_pos.x + (icon_size.width / 2.0) - (window_size.width as f64 / 2.0);
    // A few points of gap below the icon, same idea as a native menu-bar dropdown.
    let y = icon_pos.y + icon_size.height + 4.0;
    let position = PhysicalPosition::new(x, y);
    let _ = window.set_position(position);
    Some(position)
}

/// Forces WKWebView to actually repaint after becoming visible, working around an intermittent
/// wry/WKWebView issue on macOS where a transparent window's content doesn't reliably recomposite
/// right after an `orderOut`/`orderFront` (hide/show) cycle -- it can keep showing stale or empty
/// content until *something* forces a real relayout. A 1-point resize-and-restore is a standard,
/// imperceptible workaround for this class of bug (see tauri-apps/wry#1524).
fn nudge_repaint(window: &tauri::WebviewWindow) {
    let Ok(size) = window.outer_size() else {
        return;
    };
    let nudged = tauri::PhysicalSize::new(size.width.saturating_sub(1), size.height);
    let _ = window.set_size(tauri::Size::Physical(nudged));
    let _ = window.set_size(tauri::Size::Physical(size));
}

fn show_main_window_near_tray(
    app: &AppHandle,
    show_state: &WindowShowState,
    tray_rect: Option<tauri::Rect>,
) {
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
    match rect {
        Some(rect) => {
            if let Some(position) = position_window_under_tray(&window, rect) {
                *show_state.last_tray_position.lock().unwrap() = Some(position);
            }
        }
        // The tray click definitely happened (we're in this function at all), but this
        // particular event/lookup didn't carry a usable rect -- reuse the last position we
        // successfully computed rather than leaving the window whatever position it last had
        // (which, before the first successful positioning, is nowhere near the tray).
        None => {
            if let Some(position) = *show_state.last_tray_position.lock().unwrap() {
                let _ = window.set_position(position);
            }
        }
    }
    let _ = window.show();
    let _ = window.set_focus();
    *show_state.last_shown_at.lock().unwrap() = Some(Instant::now());
    nudge_repaint(&window);
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
            "show" => {
                let show_state = app.state::<WindowShowState>();
                show_main_window_near_tray(app, &show_state, None);
            }
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
                let app = tray.app_handle();
                let show_state = app.state::<WindowShowState>();
                show_main_window_near_tray(app, &show_state, Some(rect));
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
                    let show_state = window.state::<WindowShowState>();
                    let recently_shown = show_state
                        .last_shown_at
                        .lock()
                        .unwrap()
                        .is_some_and(|at| at.elapsed() < POST_SHOW_BLUR_GUARD);
                    if !recently_shown {
                        let _ = window.hide();
                    }
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

            // Without this, macOS's Automatic Termination silently kills the whole app after a
            // while: it deliberately keeps its main window hidden between tray clicks (that's the
            // entire point of a menu-bar popover), and macOS reads "accessory-policy app with no
            // visible windows" as an idle background process safe to reap. Confirmed live via
            // Console logs -- "AutomaticTermination: No windows open yet" followed by a clean
            // voluntary exit (code 0, no crash) a short while later, with no user action at all.
            #[cfg(target_os = "macos")]
            objc2_foundation::NSProcessInfo::processInfo().disableAutomaticTermination(
                &objc2_foundation::NSString::from_str(
                    "Menu-bar app must keep running with its window hidden",
                ),
            );

            app.manage(WindowShowState::default());
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
