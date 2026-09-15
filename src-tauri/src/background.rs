//! The static Windows wallpaper underneath us.
//!
//! Nothing of ours needs it - our windows cover it completely - but anything
//! that derives its own translucency from the wallpaper bitmap does. Stardock
//! Fences is the obvious case: it tints its panels with a blurred sample of the
//! wallpaper image, so with a photo behind the animation the panels end up
//! showing colours that are nowhere on screen. Painting the static wallpaper
//! flat black puts those panels back in step with what is actually visible.

use std::fs;
use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_GETDESKWALLPAPER,
    SPI_SETDESKWALLPAPER,
};

use crate::desktop;

const FILE_NAME: &str = "flat-background.bmp";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn current() -> Option<String> {
    let mut buffer = [0u16; 520];

    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buffer.len() as u32,
            buffer.as_mut_ptr().cast(),
            0,
        )
    };

    if ok == 0 {
        return None;
    }

    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    let path = String::from_utf16_lossy(&buffer[..end]);

    (!path.is_empty()).then_some(path)
}

fn apply(path: &str) -> bool {
    let path = wide(path);

    unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            path.as_ptr() as *mut _,
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        ) != 0
    }
}

/// Writes a black bitmap covering the whole virtual desktop, so it reads as
/// solid black under every wallpaper fit setting rather than only under Fill.
fn write_black_bitmap(dir: &Path) -> std::io::Result<PathBuf> {
    let screen = desktop::virtual_screen();
    let width = screen.width.max(1);
    let height = screen.height.max(1);

    let stride = (width as usize * 3).div_ceil(4) * 4;
    let pixels = stride * height as usize;
    let offset = 14 + 40;

    let mut file = Vec::with_capacity(offset + pixels);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((offset + pixels) as u32).to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&(offset as u32).to_le_bytes());

    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&width.to_le_bytes());
    file.extend_from_slice(&height.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&24u16.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&(pixels as u32).to_le_bytes());
    file.extend_from_slice(&2835i32.to_le_bytes());
    file.extend_from_slice(&2835i32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());

    file.resize(offset + pixels, 0);

    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    fs::write(&path, file)?;

    Ok(path)
}

pub fn is_ours(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(FILE_NAME)
}

/// Switches the static wallpaper to black and returns what was there before,
/// unless it is already ours.
pub fn set_black(dir: &Path) -> Option<String> {
    let previous = current().filter(|path| !is_ours(path));

    match write_black_bitmap(dir) {
        Ok(path) => {
            apply(&path.to_string_lossy());
        }
        Err(error) => eprintln!("could not write the flat background: {error}"),
    }

    previous
}

pub fn restore(previous: Option<&str>) {
    // An empty path clears the wallpaper, which is the right answer when there
    // was none to begin with.
    apply(previous.unwrap_or(""));
}
