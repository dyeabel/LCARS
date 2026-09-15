//! Turning ordinary windows into the desktop wallpaper.
//!
//! Explorer paints the wallpaper into a WorkerW window that sits behind the
//! desktop icon host (SHELLDLL_DefView). Poking Progman with 0x052C makes it
//! create that window; re-parenting our own windows into it puts them exactly
//! where the wallpaper bitmap would be - behind the icons, out of Alt+Tab and
//! off the taskbar.
//!
//! Where the WorkerW ends up depends on the Windows build:
//!   Win10 / early Win11 - a top-level window right after Progman in z-order
//!   Win11 23H2+         - a child of Progman, below SHELLDLL_DefView
//! Both layouts are handled here, with Progman itself as a last resort.

use std::ffi::c_void;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{BOOL, FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows_sys::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, GetMonitorInfoW, InvalidateRect, MonitorFromPoint,
    SetRectRgn, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULLREGION, RGN_DIFF,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, GetForegroundWindow, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindow, IsWindowVisible, SendMessageTimeoutW,
    SetParent, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_BOTTOM, SMTO_NORMAL,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

const WM_SPAWN_WORKER: u32 = 0x052c;
/// Explorer creates the WorkerW asynchronously, and takes its time about it
/// right after a restart.
const SPAWN_SETTLE: Duration = Duration::from_millis(60);
const SPAWN_TIMEOUT: Duration = Duration::from_millis(900);

/// Parameter pairs different Windows builds want before they hand out a WorkerW.
const SPAWN_VARIANTS: [(usize, isize); 3] = [(0, 0), (0x0d, 0x01), (0x0d, 0x00)];

/// Shell windows that must never be mistaken for an application covering the
/// desktop. Progman and WorkerW span the whole thing by definition - they are
/// the desktop - and the rest are shell surfaces the wallpaper lives happily
/// underneath.
const SHELL_CLASSES: [&str; 8] = [
    "Progman",
    "WorkerW",
    "SHELLDLL_DefView",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
];

/// Windows 11 rounds window corners and GetWindowRect counts the invisible
/// resize border, so a maximised window never quite reaches the edges of the
/// screen. Pulling the test area in by this much keeps a few stray pixels from
/// reading as "the desktop is still showing".
const COVERAGE_SLACK: i32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostMode {
    /// Parented into a WorkerW, which already sits below the icons.
    WorkerW,
    /// Parented into Progman itself; we have to stay at the bottom by hand.
    Progman,
}

/// The desktop window our wallpaper windows live inside.
#[derive(Clone, Copy, Debug)]
pub struct Host {
    pub hwnd: isize,
    pub progman: isize,
    pub mode: HostMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn as_hwnd(handle: isize) -> HWND {
    handle as HWND
}

fn find_window(class: &str) -> isize {
    let class = wide(class);
    unsafe { FindWindowExW(std::ptr::null_mut(), std::ptr::null_mut(), class.as_ptr(), std::ptr::null()) as isize }
}

fn find_child(parent: isize, class: &str) -> isize {
    let class = wide(class);
    unsafe { FindWindowExW(as_hwnd(parent), std::ptr::null_mut(), class.as_ptr(), std::ptr::null()) as isize }
}

/// FindWindowEx across top-level windows: the next `class` window after `after`.
fn find_sibling_after(after: isize, class: &str) -> isize {
    let class = wide(class);
    unsafe { FindWindowExW(std::ptr::null_mut(), as_hwnd(after), class.as_ptr(), std::ptr::null()) as isize }
}

pub fn class_name(handle: isize) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(as_hwnd(handle), buffer.as_mut_ptr(), buffer.len() as i32) };

    if length <= 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..length as usize])
}

pub fn window_rect(handle: isize) -> Option<Rect> {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };

    if unsafe { GetWindowRect(as_hwnd(handle), &mut rect) } != TRUE {
        return None;
    }

    Some(Rect {
        x: rect.left,
        y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    })
}

pub fn virtual_screen() -> Rect {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// The part of a monitor the shell leaves free - everything except the taskbar
/// and any other appbars docked to its edges.
pub fn work_area(bounds: Rect) -> Rect {
    let point = POINT {
        x: bounds.x + bounds.width / 2,
        y: bounds.y + bounds.height / 2,
    };

    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return bounds;
    }

    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;

    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return bounds;
    }

    let work = info.rcWork;
    Rect {
        x: work.left,
        y: work.top,
        width: work.right - work.left,
        height: work.bottom - work.top,
    }
}

/// Collects the window that owns the desktop icons - Progman or a top-level WorkerW.
unsafe extern "system" fn icon_host_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = &mut *(lparam as *mut isize);
    let handle = hwnd as isize;

    if find_child(handle, "SHELLDLL_DefView") == 0 {
        return TRUE;
    }

    *found = handle;
    0
}

fn find_icon_host() -> isize {
    let mut found: isize = 0;
    unsafe { EnumWindows(Some(icon_host_proc), &mut found as *mut isize as LPARAM) };
    found
}

fn spans_virtual_screen(handle: isize) -> bool {
    let virtual_screen = virtual_screen();

    window_rect(handle)
        .map(|rect| rect.width >= virtual_screen.width - 2 && rect.height >= virtual_screen.height - 2)
        .unwrap_or(false)
}

/// The wallpaper surface, or 0 if Explorer has not made one yet.
pub fn find_worker_w() -> isize {
    let icon_host = find_icon_host();
    if icon_host == 0 {
        return 0;
    }

    // Win10 layout: a top-level WorkerW right behind the icon host. Plenty of
    // unrelated WorkerW windows exist, so this one has to cover the desktop.
    let sibling = find_sibling_after(icon_host, "WorkerW");
    if sibling != 0 && spans_virtual_screen(sibling) {
        return sibling;
    }

    // Win11 layout: a WorkerW nested inside the icon host. Nothing else is
    // shaped like that, so it needs no size check - and it can still be
    // catching up with the desktop size right after an Explorer restart.
    find_child(icon_host, "WorkerW")
}

fn wait_for_worker_w() -> isize {
    let deadline = Instant::now() + SPAWN_TIMEOUT;

    loop {
        let worker = find_worker_w();
        if worker != 0 || Instant::now() >= deadline {
            return worker;
        }

        thread::sleep(SPAWN_SETTLE);
    }
}

pub fn resolve_host() -> Option<Host> {
    let progman = find_window("Progman");
    if progman == 0 {
        return None;
    }

    let mut worker = find_worker_w();

    for (wparam, lparam) in SPAWN_VARIANTS {
        if worker != 0 {
            break;
        }

        unsafe {
            SendMessageTimeoutW(
                as_hwnd(progman),
                WM_SPAWN_WORKER,
                wparam,
                lparam,
                SMTO_NORMAL,
                1000,
                std::ptr::null_mut(),
            )
        };

        worker = wait_for_worker_w();
    }

    if worker != 0 {
        return Some(Host { hwnd: worker, progman, mode: HostMode::WorkerW });
    }

    // No WorkerW anywhere. Parenting into Progman still works as long as we
    // stay pinned to the bottom of its z-order so the icons paint on top.
    Some(Host { hwnd: progman, progman, mode: HostMode::Progman })
}

pub fn host_alive(host: &Host) -> bool {
    unsafe { IsWindow(as_hwnd(host.hwnd)) == TRUE }
}

pub fn attach(window: isize, host: &Host) -> bool {
    unsafe { !SetParent(as_hwnd(window), as_hwnd(host.hwnd)).is_null() }
}

/// Position a wallpaper window over one monitor. Child coordinates are relative
/// to the host's client area, and the host spans the whole virtual screen, so
/// monitor positions need the virtual origin removed.
pub fn place(window: isize, bounds: Rect, host: &Host) {
    let origin = virtual_screen();

    let (insert_after, flags) = match host.mode {
        HostMode::Progman => (HWND_BOTTOM, SWP_NOACTIVATE | SWP_SHOWWINDOW),
        HostMode::WorkerW => (std::ptr::null_mut::<c_void>() as HWND, SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOZORDER),
    };

    unsafe {
        SetWindowPos(
            as_hwnd(window),
            insert_after,
            bounds.x - origin.x,
            bounds.y - origin.y,
            bounds.width,
            bounds.height,
            flags,
        )
    };
}

pub fn show_without_activating(window: isize) {
    unsafe { ShowWindow(as_hwnd(window), SW_SHOWNOACTIVATE) };
}

/// The frame the user actually sees. GetWindowRect hands back the layout
/// rectangle, which on a modern Windows includes an invisible resize border a
/// few pixels wide on three sides; counting that as covered desktop would make
/// windows look bigger than they are.
fn visible_rect(handle: isize) -> Option<Rect> {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };

    let result = unsafe {
        DwmGetWindowAttribute(
            as_hwnd(handle),
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut rect as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };

    if result < 0 {
        return window_rect(handle);
    }

    Some(Rect {
        x: rect.left,
        y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    })
}

/// Windows that are on screen in the window manager's books but painted
/// nowhere: suspended store apps, and the hidden helper windows a lot of
/// applications keep around.
fn is_cloaked(handle: isize) -> bool {
    let mut cloaked: u32 = 0;

    let result = unsafe {
        DwmGetWindowAttribute(
            as_hwnd(handle),
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };

    result >= 0 && cloaked != 0
}

/// Whether a window hides whatever is behind it. Layered and click-through
/// windows may be part transparent - a Rainmeter skin, a Fences panel, an
/// overlay - and the safe reading is that they do not.
fn is_opaque(handle: isize) -> bool {
    let style = unsafe { GetWindowLongPtrW(as_hwnd(handle), GWL_EXSTYLE) } as u32;
    style & (WS_EX_LAYERED | WS_EX_TRANSPARENT) == 0
}

/// Running total for the coverage walk below.
struct Coverage {
    /// What is left of the wallpaper after the windows found so far.
    region: isize,
    /// A rectangle region reused for every window, so the walk allocates once.
    scratch: isize,
    own: Vec<isize>,
    covered: bool,
}

/// Subtracts one window from what is left of the wallpaper. EnumWindows runs
/// front to back, so the walk can stop the moment nothing is left.
unsafe extern "system" fn coverage_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam as *mut Coverage);
    let handle = hwnd as isize;

    // Cheapest tests first: this runs over every top-level window several times
    // a second, and the DWM ones below are calls into another process. Most
    // windows on a desktop are invisible, so that check alone clears the field.
    if IsWindowVisible(hwnd) != TRUE
        || IsIconic(hwnd) == TRUE
        || state.own.contains(&handle)
        || !is_opaque(handle)
        || SHELL_CLASSES.contains(&class_name(handle).as_str())
        || is_cloaked(handle)
    {
        return TRUE;
    }

    let Some(rect) = visible_rect(handle) else { return TRUE };
    if rect.width <= 0 || rect.height <= 0 {
        return TRUE;
    }

    SetRectRgn(
        state.scratch as _,
        rect.x,
        rect.y,
        rect.x + rect.width,
        rect.y + rect.height,
    );

    if CombineRgn(state.region as _, state.region as _, state.scratch as _, RGN_DIFF) == NULLREGION {
        state.covered = true;
        return FALSE;
    }

    TRUE
}

/// True while other windows between them leave none of `bounds` showing.
///
/// This is the case for most of a working day - one maximised window is enough
/// - and it is the difference between the wallpaper costing a few cores and
/// costing nothing at all.
pub fn covered_by_windows(bounds: Rect, own: &[isize]) -> bool {
    let left = bounds.x + COVERAGE_SLACK;
    let top = bounds.y + COVERAGE_SLACK;
    let right = bounds.x + bounds.width - COVERAGE_SLACK;
    let bottom = bounds.y + bounds.height - COVERAGE_SLACK;

    if right <= left || bottom <= top {
        return false;
    }

    let mut state = Coverage {
        region: unsafe { CreateRectRgn(left, top, right, bottom) } as isize,
        scratch: unsafe { CreateRectRgn(0, 0, 1, 1) } as isize,
        own: own.to_vec(),
        covered: false,
    };

    if state.region == 0 || state.scratch == 0 {
        unsafe {
            if state.region != 0 {
                DeleteObject(state.region as _);
            }
            if state.scratch != 0 {
                DeleteObject(state.scratch as _);
            }
        }
        return false;
    }

    unsafe {
        EnumWindows(Some(coverage_proc), &mut state as *mut Coverage as LPARAM);
        DeleteObject(state.region as _);
        DeleteObject(state.scratch as _);
    }

    state.covered
}

/// True while a real application covers the given monitor edge to edge.
pub fn covered_by_fullscreen_app(bounds: Rect, own: &[isize]) -> bool {
    let foreground = unsafe { GetForegroundWindow() } as isize;

    if foreground == 0 || own.contains(&foreground) {
        return false;
    }

    if SHELL_CLASSES.contains(&class_name(foreground).as_str()) {
        return false;
    }

    let Some(rect) = window_rect(foreground) else {
        return false;
    };

    let slack = 2;
    rect.x <= bounds.x + slack
        && rect.y <= bounds.y + slack
        && rect.x + rect.width >= bounds.x + bounds.width - slack
        && rect.y + rect.height >= bounds.y + bounds.height - slack
}

/// Ask Explorer to repaint the desktop, e.g. after we detach.
pub fn refresh(host: &Host) {
    unsafe {
        InvalidateRect(as_hwnd(host.progman), std::ptr::null(), TRUE);

        if host.mode == HostMode::WorkerW {
            InvalidateRect(as_hwnd(host.hwnd), std::ptr::null(), TRUE);
        }
    }
}
