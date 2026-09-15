use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::bridge::{self, Viewport};
use crate::desktop::{self, Host, HostMode, Rect};
use crate::render;
use crate::settings::{Fit, Layout, Monitors, Settings};
use crate::stats::Stats;

const REPIN_DELAYS_MS: [u64; 3] = [250, 1000, 3000];
const LABEL_PREFIX: &str = "wallpaper-";
const TICK: Duration = Duration::from_millis(1500);
/// The coverage check runs on its own, faster clock: the supervisor's pace is
/// set by how quickly a lost desktop host has to be noticed, but this one
/// decides how long the desktop stays blank after the last window is moved off
/// it, and that is something the user watches.
const COVERAGE_TICK: Duration = Duration::from_millis(400);

/// One wallpaper window, bound to one monitor (or to all of them in span layout).
pub struct Screen {
    pub label: String,
    pub key: String,
    pub name: String,
    pub bounds: Rect,
    pub monitor: Rect,
    pub hwnd: isize,
    pub module: String,
    /// Not showing, and therefore not rendering either.
    pub hidden: bool,
    /// A hidden window that had to be woken to receive a dispatch, and is owed
    /// a suspend again on the next tick.
    pub awake: bool,
    pub red_alert: bool,
    pub margin: u8,
}

pub struct State {
    pub settings: Settings,
    pub settings_path: PathBuf,
    pub host: Option<Host>,
    pub screens: Vec<Screen>,
    pub signature: Vec<(String, Rect, u32)>,
    pub paused: bool,
    pub last_shuffle: Instant,
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

pub struct AppState {
    pub inner: Mutex<State>,
    pub stats: Stats,
}

impl AppState {
    pub fn new(settings: Settings, settings_path: PathBuf) -> Self {
        Self {
            inner: Mutex::new(State {
                settings,
                settings_path,
                host: None,
                screens: Vec::new(),
                signature: Vec::new(),
                paused: false,
                last_shuffle: Instant::now(),
            }),
            stats: Stats::new(),
        }
    }
}

struct Target {
    key: String,
    name: String,
    /// Where the window goes, after the screen fit and the insets.
    bounds: Rect,
    /// The whole monitor. Pausing keys off this, not off the window: a merely
    /// maximised window covers the work area and is no reason to stop.
    monitor: Rect,
    scale: f64,
}

fn union(rects: &[Rect]) -> Rect {
    let left = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let top = rects.iter().map(|r| r.y).min().unwrap_or(0);
    let right = rects.iter().map(|r| r.x + r.width).max().unwrap_or(0);
    let bottom = rects.iter().map(|r| r.y + r.height).max().unwrap_or(0);

    Rect { x: left, y: top, width: right - left, height: bottom - top }
}

/// Monitors the wallpaper should cover, in physical pixels on the virtual desktop.
fn targets(app: &AppHandle, settings: &Settings) -> Vec<Target> {
    let monitors = app.available_monitors().unwrap_or_default();
    let primary_name = app
        .primary_monitor()
        .ok()
        .flatten()
        .and_then(|monitor| monitor.name().cloned());

    let mut selected: Vec<Target> = monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let key = monitor.name().cloned().unwrap_or_else(|| format!("display-{index}"));
            let position = monitor.position();
            let size = monitor.size();

            let full = Rect {
                x: position.x,
                y: position.y,
                width: size.width as i32,
                height: size.height as i32,
            };

            // Keeping clear of the taskbar means fitting into the work area; the
            // insets then trim anything the shell does not know about.
            let bounds = match settings.fit {
                Fit::Full => full,
                Fit::WorkArea => desktop::work_area(full),
            };

            Target {
                name: format!("{key}  ({}x{})", size.width, size.height),
                key,
                bounds: settings.insets.apply(bounds),
                monitor: full,
                scale: monitor.scale_factor(),
            }
        })
        .collect();

    match &settings.monitors {
        Monitors::All => {}
        Monitors::Primary => {
            selected.retain(|target| Some(&target.key) == primary_name.as_ref());
        }
        Monitors::Only(names) => {
            selected.retain(|target| names.contains(&target.key));
        }
    }

    if selected.is_empty() {
        selected = vec![Target {
            key: "primary".into(),
            name: "Primary".into(),
            bounds: desktop::virtual_screen(),
            monitor: desktop::virtual_screen(),
            scale: app
                .primary_monitor()
                .ok()
                .flatten()
                .map(|monitor| monitor.scale_factor())
                .unwrap_or(1.0),
        }];
    }

    if settings.layout == Layout::Span {
        let bounds = union(&selected.iter().map(|target| target.bounds).collect::<Vec<_>>());
        let monitor = union(&selected.iter().map(|target| target.monitor).collect::<Vec<_>>());
        let scale = selected.first().map(|target| target.scale).unwrap_or(1.0);
        return vec![Target { key: "span".into(), name: "All screens".into(), bounds, monitor, scale }];
    }

    selected
}

/// What the windows would look like right now, so a changed monitor
/// arrangement - a new resolution, a scaling change, a screen plugged in - can
/// be spotted without waiting for the user to reload.
fn layout_signature(targets: &[Target]) -> Vec<(String, Rect, u32)> {
    targets
        .iter()
        .map(|target| (target.key.clone(), target.bounds, (target.scale * 100.0).round() as u32))
        .collect()
}

fn current_signature(app: &AppHandle, settings: &Settings) -> Vec<(String, Rect, u32)> {
    layout_signature(&targets(app, settings))
}

pub fn start(app: &AppHandle) {
    let state = app.state::<AppState>();

    let (settings, host) = {
        let mut guard = state.inner.lock().expect("wallpaper state poisoned");
        guard.host = desktop::resolve_host();
        (guard.settings.clone(), guard.host)
    };

    let Some(host) = host else {
        eprintln!("no desktop host window found - is Explorer running?");
        return;
    };

    let targets = targets(app, &settings);
    let scene = union(&targets.iter().map(|target| target.bounds).collect::<Vec<_>>());
    // Labels are never reused: an Explorer restart takes the windows with it and
    // Tauri hangs on to the label even though the HWND is long gone.
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);

    println!(
        "desktop host {:?} 0x{:x}, {} screen(s), layout {:?}",
        host.mode,
        host.hwnd,
        targets.len(),
        settings.layout
    );
    let shared_pick = crate::settings::random_module();
    let mut screens = Vec::new();

    for (index, target) in targets.iter().enumerate() {
        let module = if settings.one_scene() {
            settings.scene_module(&shared_pick)
        } else {
            settings.module_for(&target.key, &shared_pick)
        };

        // Rasterising below 1.0 lowers the page's devicePixelRatio to match, so
        // the split layout has to convert physical pixels to CSS ones through
        // the same figure or the slices land in the wrong place.
        let render_scale = settings.render_scale();

        let viewport = (settings.layout == Layout::Split).then(|| Viewport {
            scene,
            offset_x: target.bounds.x - scene.x,
            offset_y: target.bounds.y - scene.y,
            scale: target.scale * render_scale,
        });

        let script = bridge::init_script(&settings, Some(&module), index == 0, viewport);
        let label = format!("{LABEL_PREFIX}{generation}-{index}");

        let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
            .title("LCARS Wallpaper")
            .decorations(false)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .skip_taskbar(true)
            .focused(false)
            .visible(false)
            .shadow(false)
            .background_color(tauri::window::Color(0, 0, 0, 255))
            .position(target.bounds.x as f64, target.bounds.y as f64)
            .inner_size(target.bounds.width as f64, target.bounds.height as f64)
            .initialization_script(&script)
            .build();

        let window = match window {
            Ok(window) => window,
            Err(error) => {
                eprintln!("could not create the wallpaper window for {}: {error}", target.name);
                continue;
            }
        };

        let hwnd = match window.hwnd() {
            Ok(handle) => handle.0 as isize,
            Err(error) => {
                eprintln!("no window handle for {}: {error}", target.name);
                continue;
            }
        };

        if !desktop::attach(hwnd, &host) {
            eprintln!("could not re-parent the wallpaper window for {}", target.name);
        }

        let _ = window.set_ignore_cursor_events(true);

        if render_scale < 1.0 {
            render::set_rasterization_scale(&window, target.scale * render_scale);
        }

        let _ = window.show();
        desktop::show_without_activating(hwnd);
        desktop::place(hwnd, target.bounds, &host);

        println!(
            "  {} -> {}x{} at {},{} (module {module}, hwnd 0x{hwnd:x})",
            target.name, target.bounds.width, target.bounds.height, target.bounds.x, target.bounds.y
        );

        screens.push(Screen {
            label,
            key: target.key.clone(),
            name: target.name.clone(),
            bounds: target.bounds,
            monitor: target.monitor,
            hwnd,
            module,
            hidden: false,
            awake: false,
            red_alert: settings.red_alert,
            margin: settings.margin,
        });
    }

    {
        let mut guard = state.inner.lock().expect("wallpaper state poisoned");
        guard.signature = layout_signature(&targets);
        guard.screens = screens;
        guard.paused = false;
        guard.last_shuffle = Instant::now();
    }

    repin_later(app.clone());
}

/// The webview resizes itself once it is shown, so the Win32 placement needs the
/// last word a few times after the windows come up.
fn repin_later(app: AppHandle) {
    thread::spawn(move || {
        for delay in REPIN_DELAYS_MS {
            thread::sleep(Duration::from_millis(delay));
            pin_all(&app);
        }
    });
}

pub fn pin_all(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(guard) = state.inner.lock() else { return };
    let Some(host) = guard.host else { return };

    for screen in &guard.screens {
        if !screen.hidden {
            desktop::place(screen.hwnd, screen.bounds, &host);
        }
    }
}

pub fn stop(app: &AppHandle) {
    let state = app.state::<AppState>();

    // Go by what Tauri still has rather than by our own list: after an Explorer
    // restart the two disagree.
    for (label, window) in app.webview_windows() {
        if label.starts_with(LABEL_PREFIX) {
            let _ = window.destroy();
        }
    }

    let mut guard = state.inner.lock().expect("wallpaper state poisoned");
    guard.screens.clear();

    if let Some(host) = guard.host {
        desktop::refresh(&host);
    }
}

pub fn rebuild(app: &AppHandle) {
    stop(app);
    start(app);
}

/// The monitor a window belongs to, for dispatching back to just that one.
pub fn key_of(app: &AppHandle, label: &str) -> Option<String> {
    app.state::<AppState>()
        .inner
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .screens
                .iter()
                .find(|screen| screen.label == label)
                .map(|screen| screen.key.clone())
        })
}

/// Push an action into one screen, or into every screen when `key` is None.
pub fn dispatch(app: &AppHandle, key: Option<&str>, action: &str, value: serde_json::Value) {
    let state = app.state::<AppState>();
    let labels: Vec<String> = {
        let Ok(guard) = state.inner.lock() else { return };
        guard
            .screens
            .iter()
            .filter(|screen| key.is_none_or(|key| screen.key == key))
            .map(|screen| screen.label.clone())
            .collect()
    };

    let script = bridge::dispatch_call(action, &value);

    for label in labels {
        let Some(window) = app.get_webview_window(&label) else { continue };

        // A suspended page would never run this. Waking it leaves it invisible
        // and so still cheap; the supervisor parks it again on the next tick.
        if let Ok(mut guard) = state.inner.lock() {
            if let Some(screen) = guard.screens.iter_mut().find(|screen| screen.label == label) {
                if screen.hidden && !screen.awake {
                    screen.awake = true;
                    render::resume(&window);
                }
            }
        }

        let _ = window.eval(&script);
    }
}

/// Shows or hides one window, browser and all.
///
/// Order matters both ways round: WebView2 only suspends a controller that is
/// already invisible, and only draws one that has been resumed.
fn set_window_running(app: &AppHandle, label: &str, running: bool) {
    let Some(window) = app.get_webview_window(label) else { return };

    if running {
        render::resume(&window);
        render::set_visible(&window, true);
        let _ = window.show();
    } else {
        let _ = window.hide();
        render::set_visible(&window, false);
        render::suspend(&window);
    }
}

/// Everything a freshly booted renderer needs to match the current settings.
pub fn apply_settings(app: &AppHandle, label: &str) {
    let state = app.state::<AppState>();
    let (settings, key) = {
        let Ok(guard) = state.inner.lock() else { return };
        let key = guard
            .screens
            .iter()
            .find(|screen| screen.label == label)
            .map(|screen| screen.key.clone());

        (guard.settings.clone(), key)
    };

    let Some(key) = key else { return };
    let key = Some(key.as_str());

    dispatch(app, key, "set-border-on-mouse-over", serde_json::Value::Bool(false));

    if settings.margin > 0 {
        dispatch(app, key, "set-margin", settings.margin.into());
    }

    if settings.filter != "none" {
        dispatch(app, key, "toggle-filter-effects", settings.filter.clone().into());
    }

    if settings.audio {
        dispatch(app, key, "audio-on", serde_json::Value::Null);
        dispatch(app, key, "set-sfx-volume", settings.sfx_volume.into());
        dispatch(app, key, "set-sbg-volume", settings.engine_volume.into());
    } else {
        dispatch(app, key, "audio-off", serde_json::Value::Null);
    }

    if settings.red_alert {
        dispatch(
            app,
            key,
            "set-red-alert",
            serde_json::json!({ "redAlertActive": true, "silentRedAlert": true }),
        );
    }
}

pub fn shuffle(app: &AppHandle) {
    let state = app.state::<AppState>();
    let picks: Vec<(String, String)> = {
        let Ok(mut guard) = state.inner.lock() else { return };
        let shared = crate::settings::random_module();
        let settings = guard.settings.clone();

        guard
            .screens
            .iter_mut()
            .map(|screen| {
                screen.module = if settings.one_scene() {
                    settings.scene_module(&shared)
                } else {
                    settings.module_for(&screen.key, &shared)
                };

                (screen.key.clone(), screen.module.clone())
            })
            .collect()
    };

    for (key, module) in picks {
        dispatch(app, Some(&key), "set-active-module", module.into());
    }
}

pub fn set_paused(app: &AppHandle, paused: bool) {
    let state = app.state::<AppState>();
    let labels: Vec<String> = {
        let Ok(mut guard) = state.inner.lock() else { return };
        guard.paused = paused;

        guard
            .screens
            .iter_mut()
            .map(|screen| {
                screen.hidden = paused;
                screen.awake = false;
                screen.label.clone()
            })
            .collect()
    };

    for label in labels {
        set_window_running(app, &label, !paused);
    }

    if !paused {
        pin_all(app);
    }
}

pub fn is_paused(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .inner
        .lock()
        .map(|guard| guard.paused)
        .unwrap_or(false)
}

/// Background supervisor: keeps the windows attached, hides them while a game or
/// other fullscreen app owns the monitor, and rotates modules on a timer.
pub fn supervise(app: AppHandle) {
    let coverage = app.clone();

    thread::spawn(move || loop {
        thread::sleep(TICK);

        let app = app.clone();
        let _ = app.run_on_main_thread({
            let app = app.clone();
            move || tick(&app)
        });
    });

    thread::spawn(move || loop {
        thread::sleep(COVERAGE_TICK);

        let app = coverage.clone();
        let _ = app.run_on_main_thread({
            let app = app.clone();
            move || coverage_tick(&app)
        });
    });
}

/// Decides, several times a second, whether each screen still has anything to
/// show. Kept apart from the supervisor tick so the wallpaper reappears the
/// moment the desktop does.
fn coverage_tick(app: &AppHandle) {
    let state = app.state::<AppState>();

    let mode = {
        let Ok(guard) = state.inner.lock() else { return };

        // A hand-paused wallpaper stays paused, and there is nothing to decide
        // until the windows exist.
        if guard.paused || guard.screens.is_empty() {
            return;
        }

        if guard.settings.pause_when_covered {
            AutoPause::WhenCovered
        } else if guard.settings.pause_on_fullscreen {
            AutoPause::OnFullscreen
        } else {
            AutoPause::Never
        }
    };

    if mode == AutoPause::Never {
        return;
    }

    update_auto_pause(app, mode);
}

fn tick(app: &AppHandle) {
    let state = app.state::<AppState>();

    let (host_lost, on_fallback_host, shuffle_due) = {
        let Ok(guard) = state.inner.lock() else { return };

        let host_lost = match guard.host {
            Some(host) => !desktop::host_alive(&host),
            None => true,
        };

        let on_fallback_host = matches!(guard.host, Some(host) if host.mode == HostMode::Progman);

        let shuffle_due = guard.settings.shuffle_minutes > 0
            && guard.last_shuffle.elapsed() >= Duration::from_secs(guard.settings.shuffle_minutes * 60);

        (host_lost, on_fallback_host, shuffle_due)
    };

    // Explorer restarting takes the wallpaper host - and every child - with it.
    if host_lost {
        rebuild(app);
        return;
    }

    // If we had to settle for Progman, move over as soon as Explorer produces a
    // proper wallpaper surface.
    if on_fallback_host && desktop::find_worker_w() != 0 {
        rebuild(app);
        return;
    }

    // Resolution changed, a screen came or went, someone moved a monitor in the
    // display settings - the windows have to be laid out again.
    let (settings, known) = {
        let Ok(guard) = state.inner.lock() else { return };
        (guard.settings.clone(), guard.signature.clone())
    };

    if current_signature(app, &settings) != known {
        // The flat background is cut to the desktop size, so it needs redoing too.
        crate::apply_static_background(app);
        rebuild(app);
        crate::tray::refresh(app);
        return;
    }

    if shuffle_due {
        shuffle(app);
        if let Ok(mut guard) = state.inner.lock() {
            guard.last_shuffle = Instant::now();
        }
    }

    // Whether a screen has anything to show is decided on its own, faster
    // clock; all this tick owes it is parking any window a dispatch had to wake.
    park_woken_screens(app);
}

/// When the wallpaper stops drawing itself.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AutoPause {
    Never,
    /// Only for an application that owns a whole monitor - a game, a video.
    OnFullscreen,
    /// For anything that leaves none of the wallpaper showing, which on a
    /// working desktop is most of the time.
    WhenCovered,
}

/// A wallpaper nobody can see still costs a CPU and a GPU, so stop drawing it.
fn update_auto_pause(app: &AppHandle, mode: AutoPause) {
    let state = app.state::<AppState>();

    let changes: Vec<(String, bool)> = {
        let Ok(mut guard) = state.inner.lock() else { return };
        let handles: Vec<isize> = guard.screens.iter().map(|screen| screen.hwnd).collect();

        guard
            .screens
            .iter_mut()
            .filter_map(|screen| {
                // Fullscreen is judged against the whole monitor - a maximised
                // window covers the work area and is no reason to stop - while
                // coverage is judged against the wallpaper itself, which is all
                // there is to see.
                let covered = match mode {
                    AutoPause::Never => false,
                    AutoPause::OnFullscreen => {
                        desktop::covered_by_fullscreen_app(screen.monitor, &handles)
                    }
                    AutoPause::WhenCovered => {
                        desktop::covered_by_windows(screen.bounds, &handles)
                    }
                };

                if covered == screen.hidden {
                    return None;
                }

                screen.hidden = covered;
                screen.awake = false;
                Some((screen.label.clone(), covered))
            })
            .collect()
    };

    if changes.is_empty() {
        return;
    }

    for (label, hidden) in changes {
        set_window_running(app, &label, !hidden);
    }

    pin_all(app);
}

/// Suspends again the hidden windows that a dispatch had to wake.
fn park_woken_screens(app: &AppHandle) {
    let state = app.state::<AppState>();

    let labels: Vec<String> = {
        let Ok(mut guard) = state.inner.lock() else { return };

        guard
            .screens
            .iter_mut()
            .filter(|screen| screen.hidden && screen.awake)
            .map(|screen| {
                screen.awake = false;
                screen.label.clone()
            })
            .collect()
    };

    for label in labels {
        if let Some(window) = app.get_webview_window(&label) {
            render::suspend(&window);
        }
    }
}
