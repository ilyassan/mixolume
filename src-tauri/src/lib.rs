mod mixer;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mixer::{AppSession, AudioMixerBackend, DuckingSettings, MixerError};
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
    /// Whether the window has ever been shown before. On macOS it's re-anchored under the
    /// menu-bar icon on *every* show (other menu extras coming and going shifts the icon, and
    /// the window has no other persistent position to speak of). On Windows/Linux it's only
    /// anchored there the first time -- the window is draggable and has a real taskbar entry
    /// there (see `tauri.conf.json`'s `skipTaskbar: false`), so re-snapping it under the tray on
    /// every open would fight the user's own placement of it.
    has_shown_before: Mutex<bool>,
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

#[tauri::command]
fn set_balance(state: State<MixerState>, session_id: String, balance: f32) -> Result<(), String> {
    state
        .backend
        .set_balance(&session_id, balance)
        .map_err(mixer_error_to_string)
}

/// Starts an OS-native window-move drag, for the frontend's draggable header (Windows/Linux
/// only -- see `App.tsx`). Deliberately not just the frontend calling the built-in
/// `getCurrentWindow().startDragging()` directly: on Windows, starting the drag posts a
/// synthetic `WM_NCLBUTTONDOWN`/`HTCAPTION` message to make the OS treat the click as if it hit
/// a real title bar, and handling that message hands focus briefly between the WebView2 child
/// control and the frame window -- confirmed live, that blip alone was enough for the
/// hide-on-blur handler below to read it as "the user clicked away" and hide the window before
/// the drag could even start. Stamping `last_shown_at` first reuses the exact same
/// `POST_SHOW_BLUR_GUARD` window `show_main_window_near_tray` uses for its own analogous
/// focus-blip-right-after-a-window-state-change case.
#[tauri::command]
fn begin_window_drag(
    window: tauri::WebviewWindow,
    show_state: State<WindowShowState>,
) -> Result<(), String> {
    *show_state.last_shown_at.lock().unwrap() = Some(Instant::now());
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_ducking_settings(state: State<MixerState>) -> DuckingSettings {
    state.backend.get_ducking_settings()
}

/// Whether auto-duck is actually implemented on this platform -- it needs each priority app's
/// raw audio content to tell speech from music, which macOS gets via Core Audio process taps and
/// Windows via WASAPI process-loopback capture (see `DuckingSettings`'s doc comment). Linux has
/// no such backend yet. The Settings UI uses this to hide the toggle entirely where it's
/// unsupported instead of showing a control that looks live but silently no-ops against the
/// trait's default methods.
#[tauri::command]
fn ducking_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

#[tauri::command]
fn set_ducking_enabled(state: State<MixerState>, enabled: bool) -> Result<(), String> {
    state
        .backend
        .set_ducking_enabled(enabled)
        .map_err(mixer_error_to_string)
}

#[tauri::command]
fn set_duck_trigger_priority(
    state: State<MixerState>,
    display_name: String,
    is_priority: bool,
) -> Result<(), String> {
    state
        .backend
        .set_duck_trigger_priority(&display_name, is_priority)
        .map_err(mixer_error_to_string)
}

/// What a completed update check found, reported to the frontend as `{ "status": "upToDate" }`
/// or `{ "status": "installed", "version": "..." }` -- distinct from a plain bool so the Settings
/// UI can tell the user something actionable instead of "check the console" (see PLAN.md's
/// auto-update section for why that's the bar).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
enum UpdateCheckOutcome {
    UpToDate,
    Installed { version: String },
}

/// Checks the GitHub-hosted `latest.json` manifest and, if a newer version exists, downloads and
/// installs it immediately -- the install only *takes effect* on the next launch (Tauri's
/// updater replaces the on-disk app bundle/installer but doesn't restart the running process),
/// so this is safe to run silently while the app is in active use. Shared by the silent
/// startup check and the frontend's manual "Check for Updates" button.
async fn run_update_check(app: AppHandle) -> Result<UpdateCheckOutcome, String> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app.updater().map_err(|err| err.to_string())?;
    match updater.check().await.map_err(|err| err.to_string())? {
        Some(update) => {
            let version = update.version.clone();
            update
                .download_and_install(|_chunk_len, _content_len| {}, || {})
                .await
                .map_err(|err| err.to_string())?;
            Ok(UpdateCheckOutcome::Installed { version })
        }
        None => Ok(UpdateCheckOutcome::UpToDate),
    }
}

#[tauri::command]
async fn check_for_updates(app: AppHandle) -> Result<UpdateCheckOutcome, String> {
    run_update_check(app).await
}

/// How often we re-poll the platform backend for session changes when nothing time-sensitive is
/// happening. WASAPI/PulseAudio/Core Audio don't give us a cheap cross-platform push notification
/// in v1, so we poll and only emit to the frontend when the list actually differs from what we
/// last sent (see `mixer::AppSession`'s `PartialEq`). 150ms rather than something even tighter:
/// each tick does real work (enumerating HAL/session objects and reading properties on each), and
/// this is a background menu-bar utility that should stay cheap to leave running -- but 150ms is
/// still functionally free on modern hardware (a handful of lightweight reads, ~7 times a second)
/// while keeping most UI-visible state close enough behind the real audio decision to not feel
/// laggy. Do not drop this to "instant"/0 as the *baseline* rate -- that trades an imperceptible
/// latency improvement for real, unnecessary CPU/battery cost on a process meant to run all day.
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// How often to poll while a duck is actively engaging or releasing -- see [`POLL_INTERVAL`]'s
/// doc comment for why that's too coarse specifically for this case. The realtime audio thread
/// smooths a duck's gain change continuously (`DuckingRuntime::SMOOTHING_PER_CALLBACK`), but at
/// `POLL_INTERVAL`'s rate the frontend only ever sees a "before" and an "after" snapshot with
/// most of the ramp already invisibly finished in between (confirmed live: at the default
/// smoothing rate, ~91% of the transition is already done within a single 150ms window) -- no
/// amount of client-side easing can make something look gradual if the actual change already
/// happened between two polls. Sampling faster during exactly this window gives the frontend
/// enough real intermediate data points to animate smoothly, without paying that cost while nothing
/// is transitioning (the loop drops back to `POLL_INTERVAL` the moment no session is currently a
/// duck trigger or being ducked).
const DUCK_TRANSITION_POLL_INTERVAL: Duration = Duration::from_millis(30);

fn spawn_session_poll_loop(app_handle: AppHandle, backend: Arc<dyn AudioMixerBackend>) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<Vec<AppSession>> = None;
        loop {
            let duck_active = last
                .as_ref()
                .is_some_and(|sessions| sessions.iter().any(|s| s.is_ducked || s.is_duck_trigger));
            let interval = if duck_active {
                DUCK_TRANSITION_POLL_INTERVAL
            } else {
                POLL_INTERVAL
            };
            tokio::time::sleep(interval).await;
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

/// Unique id for the (single) tray icon, so the "Show MiXolume" menu item can look it up via
/// [`tauri::Manager::tray_by_id`] and read its current on-screen position -- the menu item click
/// handler only gets an `AppHandle`, not the `&TrayIcon` a direct tray-icon click gives for free.
const TRAY_ICON_ID: &str = "mixolume-tray";

/// The monitor (if any) whose bounds contain the given physical point -- used to find which
/// screen the tray icon is actually on, since a multi-monitor setup means `primary_monitor()`
/// or the window's own (not-yet-positioned) `current_monitor()` can easily be the wrong one.
fn monitor_containing(
    window: &tauri::WebviewWindow,
    x: f64,
    y: f64,
) -> Option<tauri::window::Monitor> {
    let monitors = window.available_monitors().ok()?;
    monitors.into_iter().find(|monitor| {
        let pos = monitor.position();
        let size = monitor.size();
        x >= pos.x as f64
            && x < pos.x as f64 + size.width as f64
            && y >= pos.y as f64
            && y < pos.y as f64 + size.height as f64
    })
}

/// Moves the main window so it appears directly next to the tray icon, like a native menu-bar
/// app (Control Center, Wi-Fi/Bluetooth menu extras, etc.) rather than wherever the window
/// manager last happened to place it. Returns the computed position so the caller can cache it
/// as the fallback for the next click, in case that one can't get a tray rect at all.
///
/// `tray_rect`'s `position`/`size` are DPI-aware (`tauri::Position`/`Size`, not raw physical
/// pixels) -- converted via the window's own `scale_factor()` before arithmetic, since mixing a
/// logical tray-icon rect with a physical window size would misplace the window on any non-1x
/// display.
///
/// Which *side* of the icon to open on is not a `target_os` fact -- it depends on which edge of
/// the screen the tray/panel sits on, which varies even within one OS (a Linux panel can be
/// top or bottom depending on the desktop environment; users occasionally move the Windows
/// taskbar too). So this looks at where the icon actually sits on its monitor: near the top
/// (macOS's menu bar) opens downward, near the bottom (the Windows/most-Linux default) opens
/// upward. Without that check, opening downward unconditionally -- correct for macOS -- pushed
/// the window off the bottom of the screen on Windows, where the window manager then clamped it
/// back on-screen at whatever fixed spot it clamps off-screen windows to, making it look "stuck"
/// in one place instead of tracking the tray icon.
fn position_window_under_tray(
    window: &tauri::WebviewWindow,
    tray_rect: tauri::Rect,
) -> Option<PhysicalPosition<f64>> {
    let scale = window.scale_factor().ok()?;
    let window_size = window.outer_size().ok()?;
    let icon_pos = tray_rect.position.to_physical::<f64>(scale);
    let icon_size = tray_rect.size.to_physical::<f64>(scale);
    let icon_center_x = icon_pos.x + icon_size.width / 2.0;
    let icon_center_y = icon_pos.y + icon_size.height / 2.0;

    // A few points of gap between the window and the icon, same idea as a native dropdown.
    const GAP: f64 = 4.0;

    let monitor = monitor_containing(window, icon_center_x, icon_center_y)
        .or_else(|| window.current_monitor().ok().flatten());

    let (y, x_bounds, y_bounds) = match &monitor {
        Some(monitor) => {
            let m_pos = monitor.position();
            let m_size = monitor.size();
            let top = m_pos.y as f64;
            let bottom = top + m_size.height as f64;
            let left = m_pos.x as f64;
            let right = left + m_size.width as f64;

            // Closer to the monitor's top edge than its bottom edge -> menu-bar style (open
            // below); otherwise -> taskbar/panel style (open above).
            let opens_downward = (icon_center_y - top) <= (bottom - icon_center_y);
            let y = if opens_downward {
                icon_pos.y + icon_size.height + GAP
            } else {
                icon_pos.y - window_size.height as f64 - GAP
            };
            (y, Some((left, right)), Some((top, bottom)))
        }
        // No monitor info at all -- fall back to the old macOS-shaped assumption rather than
        // leaving the window unpositioned.
        None => (icon_pos.y + icon_size.height + GAP, None, None),
    };

    let mut x = icon_center_x - (window_size.width as f64 / 2.0);
    let mut y = y;
    // Keep the window fully on-screen horizontally/vertically -- relevant for icons near a
    // screen edge/corner, where centering under the icon alone could hang part of the window
    // off the monitor.
    if let Some((left, right)) = x_bounds {
        let max_x = (right - window_size.width as f64).max(left);
        x = x.clamp(left, max_x);
    }
    if let Some((top, bottom)) = y_bounds {
        let max_y = (bottom - window_size.height as f64).max(top);
        y = y.clamp(top, max_y);
    }

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

/// "Get it out of the way" for this platform: fully hidden on macOS (the menu-bar-popover
/// convention -- Control Center, Wi-Fi/Bluetooth extras, etc. -- which also has no Dock/taskbar
/// entry to preserve), minimized everywhere else (Windows/Linux now have a real, persistent
/// taskbar entry -- `tauri.conf.json`'s `skipTaskbar: false` -- and `hide()` would make that
/// entry disappear too, since a hidden top-level window has no taskbar button; minimizing keeps
/// the button while getting the window off-screen, exactly like Windows' own Volume Mixer/Action
/// Center flyouts).
fn dismiss_window(window: &tauri::WebviewWindow) {
    if cfg!(target_os = "macos") {
        let _ = window.hide();
    } else {
        let _ = window.minimize();
    }
}

/// The inverse of [`dismiss_window`].
fn restore_window(window: &tauri::WebviewWindow) {
    if !cfg!(target_os = "macos") {
        let _ = window.unminimize();
    }
    let _ = window.show();
    let _ = window.set_focus();
    activate_app_macos();
}

/// Explicitly makes the whole *application* (not just this window) the active/foreground app --
/// macOS only, a no-op everywhere else. Needed specifically because MiXolume runs with
/// `NSApplicationActivationPolicy::Accessory` (no Dock icon, matching Control Center/menu-bar-
/// extra convention -- see `setup_tray`'s doc comment) -- an accessory app doesn't automatically
/// become the active app just because one of its windows is shown, unlike a normal Dock app.
/// Confirmed live: without this, `window.show()` + `window.set_focus()` alone leaves the *window*
/// key but the *app* still backgrounded from WebKit's perspective, and WKWebView throttles a
/// backgrounded webview's rendering (including `requestAnimationFrame`-driven UI animations) even
/// while it's visibly on screen -- exactly the "duck/volume transitions look instant instead of
/// smooth, only on macOS" symptom this was added to rule out.
///
/// Deliberately the older, deprecated `activateIgnoringOtherApps(true)` over the newer
/// `activate()`: Apple's own docs for `activate()` say explicitly "the framework does not
/// guarantee the app will be activated at all" (it supports cooperative yielding another app can
/// decline), which isn't acceptable for a menu-bar utility that must reliably come to the
/// foreground the instant its tray icon is clicked. `activateIgnoringOtherApps(true)` has no such
/// escape hatch.
fn activate_app_macos() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;

        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

fn show_main_window_near_tray(
    app: &AppHandle,
    show_state: &WindowShowState,
    tray_rect: Option<tauri::Rect>,
) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // A minimized window still reports `is_visible() == true` in Win32 terms (visibility and
    // iconic/minimized state are independent) -- `!is_minimized()` is what actually means "on
    // screen right now" there. Harmless on macOS, which never minimizes this window at all.
    let is_open = window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false);
    if is_open {
        dismiss_window(&window);
        return;
    }

    let mut has_shown_before = show_state.has_shown_before.lock().unwrap();
    if cfg!(target_os = "macos") || !*has_shown_before {
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
    }
    *has_shown_before = true;
    drop(has_shown_before);

    restore_window(&window);
    *show_state.last_shown_at.lock().unwrap() = Some(Instant::now());
    nudge_repaint(&window);
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show MiXolume", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            // `app.exit()` calls `std::process::exit()` and does not run `Drop` for managed
            // state, so the mixer backend's OS-level cleanup (unmuting any macOS-tapped app's
            // normal output path) needs to happen explicitly and synchronously first -- see
            // `AudioMixerBackend::shutdown`'s doc comment.
            "quit" => {
                app.state::<MixerState>().backend.shutdown();
                app.exit(0);
            }
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
    // Default to `info`+ for this crate specifically (not every dependency -- those stay at
    // `warn` unless `RUST_LOG` overrides it) so `npm run tauri dev`'s terminal actually shows
    // this app's own `log::info!`/`log::warn!` calls, including auto-duck's capture
    // activation/failure diagnostics, without needing `RUST_LOG` set by hand every time.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,mixolume_lib=info"),
    )
    .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Uses a real macOS Launch Agent (not an AppleScript login-item hack) -- the frontend
        // toggles it via the `autostart:default` capability's enable/disable/isEnabled commands.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Auto-hide on focus loss, like a native menu-bar popover (Control Center, Wi-Fi/
        // Bluetooth menu extras): clicking anywhere outside the window closes it instead of
        // leaving it stranded on screen behind whatever the user clicked into next.
        //
        // macOS only: it's the only platform where this window has no taskbar/Dock entry to
        // fall back on, so hiding it on any ordinary click-away is the only way to get it out of
        // the way at all -- matching how every native macOS menu extra behaves. Windows/Linux
        // now have a real taskbar entry (`tauri.conf.json`'s `skipTaskbar: false`) and are
        // draggable, so they behave like a normal utility window instead: it stays open across
        // focus changes and is dismissed by explicitly minimizing it (via the tray icon or the
        // taskbar itself, see `dismiss_window`), not by clicking elsewhere.
        .on_window_event(|window, event| {
            if cfg!(target_os = "macos") && window.label() == "main" {
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
            // no Cmd+Tab entry. This is entirely `ActivationPolicy`, not `skipTaskbar` -- confirmed
            // via Tauri's own doc comment on `set_skip_taskbar` ("macOS: Unsupported"), so
            // tauri.conf.json's `skipTaskbar: false` (needed so Windows/Linux show a normal
            // taskbar entry, unlike macOS's menu-bar-only convention) has no effect here either
            // way; the Dock icon is controlled purely by the activation policy, which defaults to
            // `Regular` (a normal Dock-visible app) unless set otherwise here.
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

            // Silent background update check, like Sparkle on macOS -- release builds only (a
            // dev build has no meaningful "latest.json" to compare against, and would just spam
            // the log). The delay lets startup (session polling, tray) finish first; a failed or
            // slow update check should never be why the app feels sluggish to open.
            #[cfg(not(debug_assertions))]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    match run_update_check(handle).await {
                        Ok(UpdateCheckOutcome::Installed { version }) => {
                            log::info!("Installed update to {version}; takes effect next launch");
                        }
                        Ok(UpdateCheckOutcome::UpToDate) => {
                            log::info!("Already on the latest version");
                        }
                        Err(err) => {
                            log::warn!("Background update check failed: {err}");
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            set_volume,
            set_muted,
            set_balance,
            begin_window_drag,
            check_for_updates,
            get_ducking_settings,
            ducking_supported,
            set_ducking_enabled,
            set_duck_trigger_priority
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
