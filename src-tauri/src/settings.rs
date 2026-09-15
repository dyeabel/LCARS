use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub const MODULES: [&str; 9] = [
    "default", "ent", "entD", "entF", "entG", "ncc1031", "warpDrive", "starbase", "dna",
];

pub const FILTERS: [&str; 4] = ["none", "softGlow", "grayScale", "lightMode"];

/// Fractions of the real pixel count the scene may be rasterised at. Rendering
/// and compositing cost follows the number of pixels, so this is the one knob
/// that lowers the price of the animation itself rather than the time it runs.
pub const RENDER_SCALES: [(&str, f64); 5] = [
    ("Full (sharpest)", 1.0),
    ("85%", 0.85),
    ("75%", 0.75),
    ("60%", 0.6),
    ("50% (cheapest)", 0.5),
];

/// How the scene is spread over the available monitors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// One independent scene per monitor, each with its own module.
    Separate,
    /// A single window covering every monitor, so the interface runs across them.
    Span,
    /// One scene the size of the whole desktop, but every monitor gets its own
    /// window showing the slice that belongs to it.
    Split,
}

impl Default for Layout {
    fn default() -> Self {
        Layout::Separate
    }
}

/// How much of a monitor the wallpaper covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// The whole monitor, taskbar included - the shell paints over the bottom.
    #[default]
    Full,
    /// Only what the shell leaves free, so the taskbar covers nothing.
    WorkArea,
}

/// Extra pixels trimmed off each edge after the fit, for docks and bars the
/// shell does not report as appbars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Insets {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

impl Insets {
    pub fn is_zero(&self) -> bool {
        *self == Insets::default()
    }

    pub fn apply(&self, bounds: crate::desktop::Rect) -> crate::desktop::Rect {
        crate::desktop::Rect {
            x: bounds.x + self.left,
            y: bounds.y + self.top,
            width: (bounds.width - self.left - self.right).max(1),
            height: (bounds.height - self.top - self.bottom).max(1),
        }
    }
}

/// What to do with the static Windows wallpaper hiding behind the animation.
/// Anything that samples it - Fences tinting its panels, for one - looks wrong
/// when it is a photo and the visible background is something else entirely.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StaticBackground {
    #[default]
    Leave,
    Black,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "names", rename_all = "lowercase")]
pub enum Monitors {
    All,
    Primary,
    Only(Vec<String>),
}

impl Default for Monitors {
    fn default() -> Self {
        Monitors::All
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub layout: Layout,
    pub monitors: Monitors,
    pub module: String,
    pub varied_modules: bool,
    pub module_by_monitor: HashMap<String, String>,
    pub margin: u8,
    pub filter: String,
    pub red_alert: bool,
    pub audio: bool,
    pub sfx_volume: f64,
    pub engine_volume: f64,
    pub boot_animation: bool,
    pub system_info: bool,
    pub pause_on_fullscreen: bool,
    /// Stop rendering whenever other windows leave none of the wallpaper
    /// showing, not just when one of them is fullscreen.
    pub pause_when_covered: bool,
    /// Fraction of the real pixels the scene is rasterised into, 1.0 for all of
    /// them. See RENDER_SCALES.
    pub render_scale: f64,
    pub shuffle_minutes: u64,
    pub fit: Fit,
    pub insets: Insets,
    pub static_background: StaticBackground,
    /// The wallpaper to put back when we stop flattening it.
    pub saved_wallpaper: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            monitors: Monitors::default(),
            module: "random".into(),
            varied_modules: true,
            module_by_monitor: HashMap::new(),
            margin: 0,
            filter: "none".into(),
            red_alert: false,
            audio: false,
            sfx_volume: 0.22,
            engine_volume: 0.12,
            boot_animation: true,
            system_info: true,
            pause_on_fullscreen: true,
            pause_when_covered: true,
            render_scale: 1.0,
            shuffle_minutes: 0,
            fit: Fit::default(),
            insets: Insets::default(),
            static_background: StaticBackground::Leave,
            saved_wallpaper: None,
        }
    }
}

impl Settings {
    pub fn load(path: &PathBuf) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                let _ = fs::write(path, json);
            }
            Err(error) => eprintln!("could not serialise settings: {error}"),
        }
    }

    /// Module for one monitor: an explicit override first, then the global
    /// choice. A random pick is rolled per monitor unless the screens are meant
    /// to stay in sync, in which case the caller passes one shared pick.
    pub fn module_for(&self, key: &str, shared: &str) -> String {
        if let Some(name) = self.module_by_monitor.get(key) {
            return name.clone();
        }

        if self.module != "random" {
            return self.module.clone();
        }

        if self.varied_modules {
            random_module()
        } else {
            shared.to_string()
        }
    }
}

impl Settings {
    /// One scene covering several screens has to use a single module, so per
    /// monitor overrides and varied random picks do not apply to it.
    pub fn scene_module(&self, shared: &str) -> String {
        if self.module == "random" {
            shared.to_string()
        } else {
            self.module.clone()
        }
    }

    pub fn one_scene(&self) -> bool {
        self.layout != Layout::Separate
    }

    /// Guards against a hand-edited settings file asking for something the
    /// browser would refuse or that would be unreadable on screen.
    pub fn render_scale(&self) -> f64 {
        if self.render_scale.is_finite() {
            self.render_scale.clamp(0.25, 1.0)
        } else {
            1.0
        }
    }
}

/// Cheap clock-seeded hash - a full RNG dependency would be overkill for
/// picking one of nine modules. The counter keeps consecutive calls apart.
pub fn random_module() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos() as u64)
        .unwrap_or(0);

    let seed = nanos ^ COUNTER.fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed);
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    state ^= state >> 33;
    state = state.wrapping_mul(0xff51afd7ed558ccd);
    state ^= state >> 33;

    MODULES[(state % MODULES.len() as u64) as usize].to_string()
}
