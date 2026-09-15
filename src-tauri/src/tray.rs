use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::settings::{
    Fit, Layout, Monitors, Settings, StaticBackground, FILTERS, MODULES, RENDER_SCALES,
};
use crate::wallpaper::{self, AppState};

pub const TRAY_ID: &str = "lcars-wallpaper";

const MODULE_LABELS: [(&str, &str); 9] = [
    ("default", "Titan (default)"),
    ("ent", "Enterprise"),
    ("entD", "Enterprise D"),
    ("entF", "Enterprise F"),
    ("entG", "Enterprise G"),
    ("ncc1031", "NCC-1031"),
    ("warpDrive", "Warp drive"),
    ("starbase", "Starbase"),
    ("dna", "DNA"),
];

const FILTER_LABELS: [(&str, &str); 4] = [
    ("none", "None"),
    ("softGlow", "Soft glow"),
    ("grayScale", "Gray scale"),
    ("lightMode", "Light mode"),
];

const VOLUME_STEPS: [(&str, f64); 6] = [
    ("100%", 1.0),
    ("60%", 0.6),
    ("36%", 0.36),
    ("22%", 0.22),
    ("12%", 0.12),
    ("Muted", 0.0),
];

const SHUFFLE_STEPS: [(&str, u64); 4] = [
    ("Off", 0),
    ("Every 15 minutes", 15),
    ("Every 30 minutes", 30),
    ("Every hour", 60),
];

fn module_label(name: &str) -> &str {
    MODULE_LABELS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, label)| *label)
        .unwrap_or(name)
}

fn settings_of(app: &AppHandle) -> Settings {
    app.state::<AppState>()
        .inner
        .lock()
        .map(|guard| guard.settings.clone())
        .unwrap_or_default()
}

fn update_settings(app: &AppHandle, edit: impl FnOnce(&mut Settings)) {
    let state = app.state::<AppState>();

    if let Ok(mut guard) = state.inner.lock() {
        edit(&mut guard.settings);
        let path = guard.settings_path.clone();
        guard.settings.save(&path);
    };
}

fn module_submenu(
    app: &AppHandle,
    title: &str,
    prefix: &str,
    current: &str,
    inherit: bool,
) -> tauri::Result<tauri::menu::Submenu<Wry>> {
    let mut builder = SubmenuBuilder::new(app, title);

    if inherit {
        builder = builder.item(
            &CheckMenuItemBuilder::with_id(format!("{prefix}inherit"), "Follow the global setting")
                .checked(current == "inherit")
                .build(app)?,
        );
        builder = builder.separator();
    }

    builder = builder.item(
        &CheckMenuItemBuilder::with_id(format!("{prefix}random"), "Random")
            .checked(current == "random")
            .build(app)?,
    );
    builder = builder.separator();

    for name in MODULES {
        builder = builder.item(
            &CheckMenuItemBuilder::with_id(format!("{prefix}{name}"), module_label(name))
                .checked(current == name)
                .build(app)?,
        );
    }

    builder.build()
}

pub fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let settings = settings_of(app);
    let state = app.state::<AppState>();

    let (screens, paused): (Vec<(String, String)>, bool) = {
        match state.inner.lock() {
            Ok(guard) => (
                guard.screens.iter().map(|screen| (screen.key.clone(), screen.name.clone())).collect(),
                guard.paused,
            ),
            Err(_) => (Vec::new(), false),
        }
    };

    let monitors: Vec<(String, String)> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let key = monitor.name().cloned().unwrap_or_else(|| format!("display-{index}"));
            let size = monitor.size();
            (key.clone(), format!("{key}  ({}x{})", size.width, size.height))
        })
        .collect();

    let mut menu = MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id("about", format!("LCARS Wallpaper {}", app.package_info().version)).enabled(false).build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("pause", if paused { "Resume" } else { "Pause" }).build(app)?)
        .item(&MenuItemBuilder::with_id("shuffle-now", "Shuffle modules now").build(app)?)
        .separator()
        .item(&module_submenu(app, "Module", "module:", &settings.module, false)?);

    // Per screen modules only make sense when the screens are separate scenes.
    if screens.len() > 1 && !settings.one_scene() {
        let mut per_screen = SubmenuBuilder::new(app, "Module per screen");

        for (key, name) in &screens {
            let current = settings.module_by_monitor.get(key).cloned().unwrap_or_else(|| "inherit".into());
            let submenu = module_submenu(app, name, &format!("screen-module:{key}:"), &current, true)?;
            per_screen = per_screen.item(&submenu);
        }

        menu = menu
            .item(&per_screen.build()?)
            .item(
                &CheckMenuItemBuilder::with_id("varied-modules", "Vary random modules between screens")
                    .checked(settings.varied_modules)
                    .build(app)?,
            );
    }

    let layout_menu = SubmenuBuilder::new(app, "Layout")
        .item(&CheckMenuItemBuilder::with_id("layout:separate", "A separate scene on each screen").checked(settings.layout == Layout::Separate).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("layout:split", "One scene split across screens").checked(settings.layout == Layout::Split).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("layout:span", "One window stretched over all screens").checked(settings.layout == Layout::Span).build(app)?)
        .build()?;

    let mut screens_menu = SubmenuBuilder::new(app, "Screens")
        .item(&CheckMenuItemBuilder::with_id("monitors:all", "All monitors").checked(settings.monitors == Monitors::All).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("monitors:primary", "Primary monitor only").checked(settings.monitors == Monitors::Primary).build(app)?);

    if monitors.len() > 1 {
        screens_menu = screens_menu.separator();

        for (key, name) in &monitors {
            let enabled = match &settings.monitors {
                Monitors::All => true,
                Monitors::Primary => false,
                Monitors::Only(names) => names.contains(key),
            };

            screens_menu = screens_menu.item(
                &CheckMenuItemBuilder::with_id(format!("monitor:{key}"), name)
                    .checked(enabled)
                    .build(app)?,
            );
        }
    }

    let mut margin_menu = SubmenuBuilder::new(app, "Margin");
    for (value, label) in [(0u8, "None"), (1, "Small"), (2, "Wide")] {
        margin_menu = margin_menu.item(
            &CheckMenuItemBuilder::with_id(format!("margin:{value}"), label)
                .checked(settings.margin == value)
                .build(app)?,
        );
    }

    let mut filter_menu = SubmenuBuilder::new(app, "Filter");
    for filter in FILTERS {
        let label = FILTER_LABELS.iter().find(|(key, _)| *key == filter).map(|(_, label)| *label).unwrap_or(filter);
        filter_menu = filter_menu.item(
            &CheckMenuItemBuilder::with_id(format!("filter:{filter}"), label)
                .checked(settings.filter == filter)
                .build(app)?,
        );
    }

    // Fewer pixels to draw is the only way to make the animation itself
    // cheaper; everything else can only stop it running.
    let mut scale_menu = SubmenuBuilder::new(app, "Render quality");
    for (label, value) in RENDER_SCALES {
        scale_menu = scale_menu.item(
            &CheckMenuItemBuilder::with_id(format!("render-scale:{value}"), label)
                .checked((settings.render_scale() - value).abs() < 0.001)
                .build(app)?,
        );
    }

    let appearance = SubmenuBuilder::new(app, "Appearance")
        .item(&margin_menu.build()?)
        .item(&filter_menu.build()?)
        .item(&scale_menu.build()?)
        .item(&CheckMenuItemBuilder::with_id("red-alert", "Red alert").checked(settings.red_alert).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("boot-animation", "Boot animation on start").checked(settings.boot_animation).build(app)?)
        .build()?;

    let mut sfx_menu = SubmenuBuilder::new(app, "Sound effects");
    let mut engine_menu = SubmenuBuilder::new(app, "Engine hum");
    for (label, value) in VOLUME_STEPS {
        sfx_menu = sfx_menu.item(
            &CheckMenuItemBuilder::with_id(format!("sfx:{value}"), label)
                .checked((settings.sfx_volume - value).abs() < f64::EPSILON)
                .enabled(settings.audio)
                .build(app)?,
        );
        engine_menu = engine_menu.item(
            &CheckMenuItemBuilder::with_id(format!("engine:{value}"), label)
                .checked((settings.engine_volume - value).abs() < f64::EPSILON)
                .enabled(settings.audio)
                .build(app)?,
        );
    }

    let audio = SubmenuBuilder::new(app, "Audio")
        .item(&CheckMenuItemBuilder::with_id("audio", "Enabled").checked(settings.audio).build(app)?)
        .separator()
        .item(&sfx_menu.build()?)
        .item(&engine_menu.build()?)
        .build()?;

    let mut shuffle_menu = SubmenuBuilder::new(app, "Shuffle modules");
    for (label, minutes) in SHUFFLE_STEPS {
        shuffle_menu = shuffle_menu.item(
            &CheckMenuItemBuilder::with_id(format!("shuffle:{minutes}"), label)
                .checked(settings.shuffle_minutes == minutes)
                .build(app)?,
        );
    }

    let settings_file = settings.insets;
    let fit_menu = SubmenuBuilder::new(app, "Screen fit")
        .item(&CheckMenuItemBuilder::with_id("fit:full", "Whole screen (taskbar covers the bottom)").checked(settings.fit == Fit::Full).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("fit:workarea", "Keep clear of the taskbar").checked(settings.fit == Fit::WorkArea).build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("insets-info", if settings_file.is_zero() {
            "Extra edges: none (set in the settings file)".to_string()
        } else {
            format!("Extra edges: {} / {} / {} / {} (T/R/B/L)", settings_file.top, settings_file.right, settings_file.bottom, settings_file.left)
        }).enabled(false).build(app)?)
        .build()?;

    // Apps that tint themselves from the wallpaper bitmap - Fences panels, for
    // one - cannot see the animation, so they need the static wallpaper flat.
    let background = SubmenuBuilder::new(app, "Static wallpaper")
        .item(&CheckMenuItemBuilder::with_id("background:leave", "Leave mine alone").checked(settings.static_background == StaticBackground::Leave).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("background:black", "Paint it black (matches Fences panels)").checked(settings.static_background == StaticBackground::Black).build(app)?)
        .build()?;

    let behaviour = SubmenuBuilder::new(app, "Behaviour")
        .item(&CheckMenuItemBuilder::with_id("pause-covered", "Pause while nothing of it is showing").checked(settings.pause_when_covered).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("pause-fullscreen", "Pause while an app runs fullscreen").checked(settings.pause_on_fullscreen).enabled(!settings.pause_when_covered).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("system-info", "Live system readouts").checked(settings.system_info).build(app)?)
        .item(&shuffle_menu.build()?)
        .item(&CheckMenuItemBuilder::with_id("autostart", "Start with Windows").checked(crate::autostart_enabled(app)).build(app)?)
        .build()?;

    menu.separator()
        .item(&layout_menu)
        .item(&screens_menu.build()?)
        .item(&appearance)
        .item(&fit_menu)
        .item(&background)
        .item(&audio)
        .item(&behaviour)
        .separator()
        .item(&MenuItemBuilder::with_id("reload", "Reload wallpaper").build(app)?)
        .item(&MenuItemBuilder::with_id("settings-file", "Settings file").build(app)?)
        .item(&MenuItemBuilder::with_id("quit", "Quit").build(app)?)
        .build()
}

pub fn refresh(app: &AppHandle) {
    let Ok(menu) = build_menu(app) else { return };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_menu(Some(menu));
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let icon = app.default_window_icon().cloned().ok_or_else(|| tauri::Error::InvalidIcon(std::io::Error::other("no bundled icon")))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("LCARS Wallpaper")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(handle_menu_event)
        .build(app)?;

    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref().to_string();
    let app = app.clone();

    match id.as_str() {
        "quit" => {
            crate::quit(&app);
            return;
        }
        "pause" => {
            let paused = wallpaper::is_paused(&app);
            wallpaper::set_paused(&app, !paused);
        }
        "shuffle-now" => wallpaper::shuffle(&app),
        "reload" => wallpaper::rebuild(&app),
        "settings-file" => open_settings_folder(&app),
        "varied-modules" => {
            update_settings(&app, |settings| settings.varied_modules = !settings.varied_modules);
        }
        "red-alert" => {
            let active = !settings_of(&app).red_alert;
            update_settings(&app, |settings| settings.red_alert = active);
            wallpaper::dispatch(
                &app,
                None,
                "set-red-alert",
                serde_json::json!({ "redAlertActive": active, "silentRedAlert": true }),
            );
        }
        "boot-animation" => {
            update_settings(&app, |settings| settings.boot_animation = !settings.boot_animation);
        }
        "audio" => {
            let enabled = !settings_of(&app).audio;
            update_settings(&app, |settings| settings.audio = enabled);
            apply_audio(&app, enabled);
        }
        "pause-covered" => {
            let enabled = !settings_of(&app).pause_when_covered;
            update_settings(&app, |settings| settings.pause_when_covered = enabled);
            // Whatever the supervisor had hidden under the old rule has to come
            // back before the new one gets a say.
            wallpaper::set_paused(&app, false);
        }
        "pause-fullscreen" => {
            let enabled = !settings_of(&app).pause_on_fullscreen;
            update_settings(&app, |settings| settings.pause_on_fullscreen = enabled);
            if !enabled {
                wallpaper::set_paused(&app, false);
            }
        }
        "system-info" => {
            update_settings(&app, |settings| settings.system_info = !settings.system_info);
        }
        "autostart" => crate::toggle_autostart(&app),
        "monitors:all" => {
            update_settings(&app, |settings| settings.monitors = Monitors::All);
            wallpaper::rebuild(&app);
        }
        "monitors:primary" => {
            update_settings(&app, |settings| settings.monitors = Monitors::Primary);
            wallpaper::rebuild(&app);
        }
        other => {
            handle_parametrised_event(&app, other);
        }
    }

    refresh(&app);
}

fn handle_parametrised_event(app: &AppHandle, id: &str) {
    if let Some(name) = id.strip_prefix("module:") {
        update_settings(app, |settings| settings.module = name.to_string());
        // Re-rolls every screen under the current layout rules: one module for a
        // shared scene, per screen picks otherwise.
        wallpaper::shuffle(app);
        return;
    }

    if let Some(rest) = id.strip_prefix("screen-module:") {
        if let Some((key, name)) = rest.rsplit_once(':') {
            let key = key.to_string();
            let name = name.to_string();

            update_settings(app, |settings| {
                if name == "inherit" {
                    settings.module_by_monitor.remove(&key);
                } else {
                    settings.module_by_monitor.insert(key.clone(), name.clone());
                }
            });

            let settings = settings_of(app);
            let shared = crate::settings::random_module();
            let module = settings.module_for(&key, &shared);
            wallpaper::dispatch(app, Some(&key), "set-active-module", module.into());
        }
        return;
    }

    if let Some(key) = id.strip_prefix("monitor:") {
        toggle_monitor(app, key);
        return;
    }

    if let Some(name) = id.strip_prefix("layout:") {
        let layout = match name {
            "span" => Layout::Span,
            "split" => Layout::Split,
            _ => Layout::Separate,
        };

        update_settings(app, |settings| settings.layout = layout);
        wallpaper::rebuild(app);
        return;
    }

    if let Some(value) = id.strip_prefix("margin:") {
        let margin: u8 = value.parse().unwrap_or(0);
        update_settings(app, |settings| settings.margin = margin);
        wallpaper::dispatch(app, None, "set-margin", margin.into());
        return;
    }

    if let Some(name) = id.strip_prefix("filter:") {
        update_settings(app, |settings| settings.filter = name.to_string());
        // The renderer treats filters as toggles, so clear before setting.
        wallpaper::dispatch(app, None, "toggle-filter-effects", serde_json::Value::Null);
        if name != "none" {
            wallpaper::dispatch(app, None, "toggle-filter-effects", name.into());
        }
        return;
    }

    if let Some(value) = id.strip_prefix("sfx:") {
        let volume: f64 = value.parse().unwrap_or(0.22);
        update_settings(app, |settings| settings.sfx_volume = volume);
        wallpaper::dispatch(app, None, "set-sfx-volume", volume.into());
        return;
    }

    if let Some(value) = id.strip_prefix("engine:") {
        let volume: f64 = value.parse().unwrap_or(0.12);
        update_settings(app, |settings| settings.engine_volume = volume);
        wallpaper::dispatch(app, None, "set-sbg-volume", volume.into());
        return;
    }

    if let Some(value) = id.strip_prefix("render-scale:") {
        let scale: f64 = value.parse().unwrap_or(1.0);
        if (settings_of(app).render_scale() - scale).abs() < 0.001 {
            return;
        }

        update_settings(app, |settings| settings.render_scale = scale);
        // The split layout maps physical pixels to CSS ones through this, and
        // the page has to be laid out again either way.
        wallpaper::rebuild(app);
        return;
    }

    if let Some(value) = id.strip_prefix("shuffle:") {
        let minutes: u64 = value.parse().unwrap_or(0);
        update_settings(app, |settings| settings.shuffle_minutes = minutes);
        return;
    }

    if let Some(choice) = id.strip_prefix("fit:") {
        let fit = if choice == "workarea" { Fit::WorkArea } else { Fit::Full };
        if fit == settings_of(app).fit {
            return;
        }

        update_settings(app, |settings| settings.fit = fit);
        wallpaper::rebuild(app);
        return;
    }

    if let Some(choice) = id.strip_prefix("background:") {
        let background = if choice == "black" { StaticBackground::Black } else { StaticBackground::Leave };
        if background == settings_of(app).static_background {
            return;
        }

        update_settings(app, |settings| settings.static_background = background);
        crate::apply_static_background(app);
        // Explorer rebuilds its desktop windows around a wallpaper change.
        wallpaper::rebuild(app);
    }
}

fn apply_audio(app: &AppHandle, enabled: bool) {
    let settings = settings_of(app);

    if !enabled {
        wallpaper::dispatch(app, None, "audio-off", serde_json::Value::Null);
        return;
    }

    wallpaper::dispatch(app, None, "audio-on", serde_json::Value::Null);
    wallpaper::dispatch(app, None, "set-sfx-volume", settings.sfx_volume.into());
    wallpaper::dispatch(app, None, "set-sbg-volume", settings.engine_volume.into());
}

fn toggle_monitor(app: &AppHandle, key: &str) {
    let all: Vec<String> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, monitor)| monitor.name().cloned().unwrap_or_else(|| format!("display-{index}")))
        .collect();

    let settings = settings_of(app);
    let mut selected: Vec<String> = match &settings.monitors {
        Monitors::Only(names) => names.clone(),
        Monitors::Primary => Vec::new(),
        Monitors::All => all.clone(),
    };

    if let Some(position) = selected.iter().position(|name| name == key) {
        selected.remove(position);
    } else {
        selected.push(key.to_string());
    }

    if selected.is_empty() {
        return;
    }

    let monitors = if selected.len() == all.len() { Monitors::All } else { Monitors::Only(selected) };
    update_settings(app, |settings| settings.monitors = monitors);
    wallpaper::rebuild(app);
}

fn open_settings_folder(app: &AppHandle) {
    let path = app
        .state::<AppState>()
        .inner
        .lock()
        .map(|guard| guard.settings_path.clone())
        .ok();

    if let Some(path) = path {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
}
