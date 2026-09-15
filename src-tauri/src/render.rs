//! Telling the browser behind a window to stop working.
//!
//! Hiding the host window is not enough. Our windows are re-parented into
//! Explorer's WorkerW, which makes them child windows, and Chromium's own
//! occlusion tracking only ever looks at top-level ones - so it never notices.
//! A hidden wallpaper window goes on animating, rasterising and compositing at
//! full speed; the cost is simply spent on pixels nobody will ever see.
//!
//! The switches that do work are on the WebView2 controller itself:
//!
//!   IsVisible = false   stops the rendering, the page keeps running
//!   TrySuspend()        stops the page too, and lets the renderer be trimmed
//!
//! Suspending needs the controller hidden first, and anything sent in with
//! `eval` while suspended would be lost, so the two are kept apart: the
//! wallpaper code resumes a window before it dispatches to it, without ever
//! making it visible again.

use tauri::WebviewWindow;
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2Controller3, ICoreWebView2_3};
use webview2_com::TrySuspendCompletedHandler;
use windows_core::Interface;

/// Whether the browser draws this window at all. Invisible stops the painting,
/// the rasterising and the compositing - the bulk of the cost - while leaving
/// the page itself alive.
pub fn set_visible(window: &WebviewWindow, visible: bool) {
    let _ = window.with_webview(move |webview| {
        let _ = unsafe { webview.controller().SetIsVisible(visible) };
    });
}

/// Parks the page: timers, animations and the renderer process with them. Only
/// legal once the controller is invisible, and WebView2 is free to refuse - a
/// download or playing audio keeps a renderer awake - in which case the
/// rendering is off regardless.
pub fn suspend(window: &WebviewWindow) {
    let _ = window.with_webview(move |webview| {
        let Ok(core) = (unsafe { webview.controller().CoreWebView2() }) else { return };
        let Ok(core) = core.cast::<ICoreWebView2_3>() else { return };

        let handler = TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())));
        let _ = unsafe { core.TrySuspend(&handler) };
    });
}

/// Wakes the page again. Harmless on one that was never suspended, and it does
/// not make the window visible - a suspended window still has to be resumed
/// before anything can be evaluated in it.
pub fn resume(window: &WebviewWindow) {
    let _ = window.with_webview(move |webview| {
        let Ok(core) = (unsafe { webview.controller().CoreWebView2() }) else { return };
        let Ok(core) = core.cast::<ICoreWebView2_3>() else { return };

        let _ = unsafe { core.Resume() };
    });
}

/// How many device pixels the scene is rasterised into. Below the monitor's own
/// scale the browser renders a smaller image and the compositor stretches it -
/// the one lever that lowers the price of the animation itself rather than the
/// time it runs, because rendering cost follows the pixel count.
///
/// The value is absolute rather than a multiplier on the monitor DPI, so the
/// caller folds the monitor's scale in.
pub fn set_rasterization_scale(window: &WebviewWindow, scale: f64) {
    let scale = scale.clamp(0.25, 4.0);

    let _ = window.with_webview(move |webview| {
        let Ok(controller) = webview.controller().cast::<ICoreWebView2Controller3>() else {
            return;
        };

        // Otherwise WebView2 puts the monitor's own scale back at the first
        // opportunity and undoes this.
        let _ = unsafe { controller.SetShouldDetectMonitorScaleChanges(false) };
        let _ = unsafe { controller.SetRasterizationScale(scale) };
    });
}
