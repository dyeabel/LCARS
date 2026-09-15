// An LCARS interface as an animated Windows wallpaper.
//
// The web app is the untouched renderer bundle from meWho's Titan.DS desktop
// app; everything around it - the desktop parenting, the tray, the per screen
// layout and the Electron style bridge the bundle expects - lives in this crate.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod background;
mod bridge;
mod desktop;
mod render;
mod settings;
mod stats;
mod tray;
mod wallpaper;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_autostart::ManagerExt;

use settings::{Settings, StaticBackground};
use wallpaper::AppState;

#[tauri::command]
fn renderer_action(
    app: AppHandle,
    window: tauri::WebviewWindow,
    action: String,
    payload: Option<serde_json::Value>,
) {
    match action.as_str() {
        "webapp-ready" => wallpaper::apply_settings(&app, window.label()),

        "request-system-info" => {
            let state = app.state::<AppState>();

            let enabled = state
                .inner
                .lock()
                .map(|guard| guard.settings.system_info)
                .unwrap_or(false);

            if !enabled {
                return;
            }

            // Answer only the window that asked. Broadcasting would wake the
            // screens that have been suspended for having nothing to show, and
            // a readout nobody is looking at is not worth a renderer; each one
            // asks again within two seconds of coming back anyway. The reading
            // itself is cached, so several screens asking costs nothing extra.
            let Some(key) = wallpaper::key_of(&app, window.label()) else { return };

            if let Some(snapshot) = state.stats.read() {
                if let Ok(value) = serde_json::to_value(snapshot) {
                    wallpaper::dispatch(&app, Some(&key), "system-info-updated", value);
                }
            }
        }

        "screen-margin-updated" => {
            let margin = payload
                .as_ref()
                .and_then(|value| value.get("marginStyle"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u8;

            update_screen(&app, window.label(), |screen| screen.margin = margin);
        }

        "red-alert-updated" => {
            let active = payload
                .as_ref()
                .and_then(|value| value.get("redAlertActive"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            update_screen(&app, window.label(), |screen| screen.red_alert = active);
        }

        _ => {}
    }
}

/// Stands in for the window state the original app exposed through its preload.
#[tauri::command]
fn window_state(app: AppHandle, window: tauri::WebviewWindow) -> serde_json::Value {
    let state = app.state::<AppState>();

    let (red_alert, margin) = state
        .inner
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .screens
                .iter()
                .find(|screen| screen.label == window.label())
                .map(|screen| (screen.red_alert, screen.margin))
        })
        .unwrap_or((false, 0));

    serde_json::json!({
        "windowType": "main",
        "titleBarStyle": "hidden",
        "redAlertActive": red_alert,
        "silentRedAlert": true,
        "marginStyle": margin,
    })
}

fn update_screen(app: &AppHandle, label: &str, edit: impl FnOnce(&mut wallpaper::Screen)) {
    if let Ok(mut guard) = app.state::<AppState>().inner.lock() {
        if let Some(screen) = guard.screens.iter_mut().find(|screen| screen.label == label) {
            edit(screen);
        }
    }
}

/// Set once the user has asked to quit. Without it the exit request below is
/// indistinguishable from the windows going away on their own.
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Shuts down for good: detaches the windows, puts the desktop back the way we
/// found it, and lets the exit request through.
pub fn quit(app: &AppHandle) {
    wallpaper::stop(app);
    restore_static_background(app);

    QUITTING.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// Undoes the flat background on the way out, if we were the ones who set it.
fn restore_static_background(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(guard) = state.inner.lock() else { return };

    if guard.settings.static_background == StaticBackground::Leave {
        return;
    }

    background::restore(guard.settings.saved_wallpaper.as_deref());
}

/// Flattens the static wallpaper, or puts the user's own back, to match the
/// current setting. Safe to call repeatedly.
pub fn apply_static_background(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(mut guard) = state.inner.lock() else { return };

    match guard.settings.static_background {
        StaticBackground::Black => {
            let directory = guard.settings_path.parent().map(Path::to_path_buf);
            let Some(directory) = directory else { return };

            if let Some(previous) = background::set_black(&directory) {
                guard.settings.saved_wallpaper = Some(previous);
            }
        }

        StaticBackground::Leave => {
            let previous = guard.settings.saved_wallpaper.take();

            // Only touch it if we were the ones who changed it.
            if previous.is_some() || background::current().is_some_and(|path| background::is_ours(&path)) {
                background::restore(previous.as_deref());
            }
        }
    }

    let path = guard.settings_path.clone();
    guard.settings.save(&path);
}

pub fn autostart_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

pub fn toggle_autostart(app: &AppHandle) {
    let manager = app.autolaunch();

    let _ = if manager.is_enabled().unwrap_or(false) {
        manager.disable()
    } else {
        manager.enable()
    };
}

fn settings_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("settings.json")
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![renderer_action, window_state])
        .setup(|app| {
            let handle = app.handle().clone();
            let path = settings_path(&handle);

            app.manage(AppState::new(Settings::load(&path), path));

            // Changing the wallpaper makes Explorer rebuild its desktop windows,
            // so it has to happen before ours exist.
            apply_static_background(&handle);

            wallpaper::start(&handle);
            tray::create(&handle)?;
            wallpaper::supervise(handle);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("could not start LCARS Wallpaper")
        .run(|_app, event| {
            // The wallpaper windows are the whole app, and losing them - to an
            // Explorer restart, say - is not a reason to quit; the supervisor
            // puts them back. Tauri routes AppHandle::exit through here too, so
            // the flag is what tells the two apart.
            if let RunEvent::ExitRequested { api, .. } = event {
                if !QUITTING.load(Ordering::SeqCst) {
                    api.prevent_exit();
                }
            }
        });
}
