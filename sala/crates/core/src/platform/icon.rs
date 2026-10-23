//! Tray icon drawn in memory (no resource file): a filled disc whose colour
//! tells the state at a glance.

use std::ffi::c_void;

use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

use super::TrayState;

pub const SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Normal,
    GameMode,
    Silenced,
}

impl IconKind {
    pub fn of(state: &TrayState) -> IconKind {
        if state.game_mode {
            IconKind::GameMode
        } else if state.muted || state.deafened {
            IconKind::Silenced
        } else {
            IconKind::Normal
        }
    }

    /// RGB.
    fn color(self) -> [u8; 3] {
        match self {
            IconKind::Normal => [0x2E, 0xC4, 0x7A],
            IconKind::GameMode => [0xF2, 0xA9, 0x3B],
            IconKind::Silenced => [0xE5, 0x48, 0x4D],
        }
    }
}

/// BGRA, top-down, premultiplied-free (straight alpha is what
/// `CreateIconIndirect` expects for 32-bit colour bitmaps).
pub fn pixels(kind: IconKind) -> Vec<u8> {
    let [r, g, b] = kind.color();
    let c = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 / 2.0 - 1.5;
    let mut out = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // 1 px anti-aliased edge
            let a = (radius + 0.5 - d).clamp(0.0, 1.0);
            // white ring inside the disc for contrast on dark and light taskbars
            let ring = (d > radius - 4.0 && d < radius - 2.0) as u8;
            let (pr, pg, pb) = if ring == 1 { (255, 255, 255) } else { (r, g, b) };
            out.extend_from_slice(&[pb, pg, pr, (a * 255.0) as u8]);
        }
    }
    out
}

/// Creates an `HICON` the caller must `DestroyIcon`. `None` if GDI refuses.
pub fn create(kind: IconKind) -> Option<HICON> {
    let px = pixels(kind);
    let mask = [0u8; SIZE * SIZE / 8]; // 1 bpp, all zero: the alpha channel decides
    // SAFETY: both buffers outlive the calls; GDI copies the bits.
    unsafe {
        let color = CreateBitmap(SIZE as i32, SIZE as i32, 1, 32, px.as_ptr() as *const c_void);
        let mask = CreateBitmap(SIZE as i32, SIZE as i32, 1, 1, mask.as_ptr() as *const c_void);
        if color.is_null() || mask.is_null() {
            if !color.is_null() {
                DeleteObject(color);
            }
            if !mask.is_null() {
                DeleteObject(mask);
            }
            return None;
        }
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        if icon.is_null() { None } else { Some(icon) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;

    fn alpha(px: &[u8], x: usize, y: usize) -> u8 {
        px[(y * SIZE + x) * 4 + 3]
    }

    #[test]
    fn disc_is_opaque_in_the_middle_and_transparent_in_corners() {
        let px = pixels(IconKind::Normal);
        assert_eq!(px.len(), SIZE * SIZE * 4);
        assert_eq!(alpha(&px, SIZE / 2, SIZE / 2), 255);
        assert_eq!(alpha(&px, 0, 0), 0);
        assert_eq!(alpha(&px, SIZE - 1, SIZE - 1), 0);
    }

    #[test]
    fn kind_follows_state_with_game_mode_first() {
        let mut s = TrayState::default();
        assert_eq!(IconKind::of(&s), IconKind::Normal);
        s.muted = true;
        assert_eq!(IconKind::of(&s), IconKind::Silenced);
        s.game_mode = true;
        assert_eq!(IconKind::of(&s), IconKind::GameMode);
        // negative control: kinds differ in colour
        assert_ne!(pixels(IconKind::Normal), pixels(IconKind::Silenced));
    }

    #[test]
    fn creates_a_real_icon() {
        let icon = create(IconKind::GameMode).expect("CreateIconIndirect");
        // SAFETY: icon was just created and is not used afterwards.
        unsafe { DestroyIcon(icon) };
    }
}
