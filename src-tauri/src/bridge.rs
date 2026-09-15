//! The Titan.DS bundle talks to an Electron preload script. This builds the
//! equivalent bridge as a WebView init script, so the untouched web app finds
//! the `electronAPI` object it expects.
//!
//! Renderer to Rust goes through Tauri's invoke; Rust to renderer goes the
//! other way through `eval`, which keeps the whole thing free of the event
//! plugin and its permission plumbing.

use serde::Serialize;

use crate::desktop::Rect;
use crate::settings::{Layout, Settings};

/// Mirrors the loading config the original preload passed through argv.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebappConfig {
    show_loading_display: bool,
    force_wait_on_loading: bool,
    gray_scale_loading_efx: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_module_name: Option<String>,
}

/// In split layout the page is laid out for the whole desktop and every window
/// shows only its own slice of it.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub scene: Rect,
    pub offset_x: i32,
    pub offset_y: i32,
    /// The monitor's DPI scale. The page works in CSS pixels, the desktop in
    /// physical ones, and at anything other than 100% the two differ.
    pub scale: f64,
}

pub fn init_script(
    settings: &Settings,
    module: Option<&str>,
    is_first: bool,
    viewport: Option<Viewport>,
) -> String {
    let config = WebappConfig {
        show_loading_display: settings.boot_animation,
        force_wait_on_loading: settings.boot_animation && is_first,
        gray_scale_loading_efx: !is_first,
        initial_module_name: module.map(str::to_string),
    };

    let config_json = serde_json::to_string(&config).unwrap_or_else(|_| "{}".into());
    let viewport_js = match (settings.layout, viewport) {
        (Layout::Split, Some(viewport)) => split_viewport_js(viewport),
        _ => String::new(),
    };

    format!(
        r#"(() => {{
  if (window.electronAPI) return;

  const config = {config_json};
  const listeners = [];

  const invoke = (command, args) => {{
    const internals = window.__TAURI_INTERNALS__;
    if (!internals) return Promise.resolve(null);
    return internals.invoke(command, args);
  }};

  window.electronBuildVersions = {{
    node: () => '',
    chrome: () => (navigator.userAgent.match(/Chrome\/([\d.]+)/) || [])[1] || '',
    electron: () => '',
  }};

  window.electronAPI = {{
    initialWebappConfig: config,
    sendElectronAction: (action, payload) => invoke('renderer_action', {{ action, payload: payload ?? null }}),
    onElectronAction: (handler) => listeners.push(handler),
    getCPUUsage: () => ({{ percentCPUUsage: 0, idleWakeupsPerSecond: 0 }}),
    invoke: () => invoke('window_state', {{}}),
  }};

  // Called from Rust with eval - the counterpart of Electron's webContents.send.
  window.__titanDispatch = (action, value) => {{
    for (const handler of listeners) {{
      try {{
        handler(action, value);
      }} catch (error) {{
        console.error('titan action failed', action, error);
      }}
    }}
  }};

{viewport_js}
}})();"#
    )
}

/// Stretches the document over the whole desktop and shifts it so this window
/// lands on its own part of the scene. The app sizes its canvas from
/// innerWidth/innerHeight and documentElement.clientWidth, so both have to
/// report the whole scene rather than this window.
///
/// The stylesheet is adopted rather than injected as a `<style>` element: the
/// CSP carries a nonce, which makes the browser ignore 'unsafe-inline' and drop
/// any style tag we create at runtime. Constructed stylesheets are not inline
/// styles, so they apply either way.
fn split_viewport_js(viewport: Viewport) -> String {
    let Viewport { scene, offset_x, offset_y, scale } = viewport;
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let css = |value: i32| (value as f64 / scale).round() as i32;

    format!(
        r#"  const sceneWidth = {width};
  const sceneHeight = {height};

  Object.defineProperty(window, 'innerWidth', {{ get: () => sceneWidth, configurable: true }});
  Object.defineProperty(window, 'innerHeight', {{ get: () => sceneHeight, configurable: true }});

  const viewportCss = `
    html, body {{
      width: ${{sceneWidth}}px !important;
      height: ${{sceneHeight}}px !important;
      max-width: none !important;
      max-height: none !important;
      overflow: hidden !important;
    }}
    html {{
      position: fixed !important;
      left: 0 !important;
      top: 0 !important;
      transform: translate({offset_x}px, {offset_y}px) !important;
      transform-origin: 0 0 !important;
    }}
  `;

  let viewportSheet = null;

  const applyViewport = () => {{
    try {{
      if (!viewportSheet) {{
        viewportSheet = new CSSStyleSheet();
        viewportSheet.replaceSync(viewportCss);
      }}

      if (!document.adoptedStyleSheets.includes(viewportSheet)) {{
        document.adoptedStyleSheets = [...document.adoptedStyleSheets, viewportSheet];
      }}
    }} catch (error) {{
      console.error('viewport override failed', error);
    }}
  }};

  // The document may still be empty when this runs and the app restyles the
  // root while it boots, so re-apply until it sticks.
  document.addEventListener('DOMContentLoaded', applyViewport);
  window.addEventListener('load', applyViewport);
  applyViewport();
  for (const delay of [0, 250, 1000, 3000]) setTimeout(applyViewport, delay);"#,
        width = css(scene.width),
        height = css(scene.height),
        offset_x = -css(offset_x),
        offset_y = -css(offset_y),
    )
}

/// A single action pushed into one window, e.g. ("set-margin", 2).
pub fn dispatch_call(action: &str, value: &serde_json::Value) -> String {
    let action = serde_json::to_string(action).unwrap_or_else(|_| "\"\"".into());
    let value = serde_json::to_string(value).unwrap_or_else(|_| "null".into());

    format!("window.__titanDispatch && window.__titanDispatch({action}, {value});")
}

