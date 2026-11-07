//! App icons: asked from the shell at exactly the pixel size we draw them,
//! turned into Direct2D bitmaps once, and kept in a small LRU cache
//! (at most 64; the least recently used one is dropped first).

use std::cell::RefCell;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HPALETTE};
use windows::Win32::Graphics::Imaging::WICBitmapUseAlpha;
use windows::Win32::UI::Shell::{IShellItem, IShellItemImageFactory, SIIGBF_ICONONLY};
use windows::core::{Interface, Result};

use crate::ui::gfx::Gfx;

const CACHE_LIMIT: usize = 64;

/// Deletes the GDI bitmap when it goes out of scope, even on early return.
struct BitmapGuard(HBITMAP);

impl Drop for BitmapGuard {
    fn drop(&mut self) {
        // SAFETY: we own this bitmap (it came from GetImage) and nobody else
        // uses it after this point.
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

#[derive(Default)]
pub struct IconCache {
    /// Oldest first. (AUMID, pixel size, bitmap).
    entries: RefCell<Vec<(String, i32, ID2D1Bitmap1)>>,
}

impl IconCache {
    /// The icon if it is already cached (marks it as recently used), or
    /// `None`. Never asks the shell, so it is always fast.
    pub fn peek(&self, aumid: &str, px: i32) -> Option<ID2D1Bitmap1> {
        let mut entries = self.entries.borrow_mut();
        let i = entries
            .iter()
            .position(|(a, s, _)| *s == px && a.eq_ignore_ascii_case(aumid))?;
        // Move to the back: most recently used.
        let entry = entries.remove(i);
        let bitmap = entry.2.clone();
        entries.push(entry);
        Some(bitmap)
    }

    /// The icon of `item` at `px` × `px` physical pixels.
    pub fn get(&self, gfx: &Gfx, aumid: &str, item: &IShellItem, px: i32) -> Result<ID2D1Bitmap1> {
        if let Some(bitmap) = self.peek(aumid, px) {
            return Ok(bitmap);
        }
        let bitmap = extract(gfx, item, px)?;
        let mut entries = self.entries.borrow_mut();
        entries.push((aumid.to_string(), px, bitmap.clone()));
        if entries.len() > CACHE_LIMIT {
            entries.remove(0);
        }
        Ok(bitmap)
    }

    /// Bitmaps belong to one GPU device; after a device loss they're useless.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
    }
}

fn extract(gfx: &Gfx, item: &IShellItem, px: i32) -> Result<ID2D1Bitmap1> {
    let factory: IShellItemImageFactory = item.cast()?;
    // SAFETY: plain COM calls. The HBITMAP is owned by the guard and deleted
    // at the end of this function; WIC copies the pixels out first.
    unsafe {
        let hbitmap = factory.GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY)?;
        let _guard = BitmapGuard(hbitmap);
        let wic =
            gfx.wic
                .CreateBitmapFromHBITMAP(hbitmap, HPALETTE::default(), WICBitmapUseAlpha)?;
        let (mut w, mut h) = (0u32, 0u32);
        wic.GetSize(&mut w, &mut h)?;
        let stride = w * 4;
        let mut pixels = vec![0u8; (stride * h) as usize];
        wic.CopyPixels(std::ptr::null(), stride, &mut pixels)?;
        premultiply_if_needed(&mut pixels);
        gfx.bitmap_from_pixels(w, h, &pixels)
    }
}

/// Direct2D wants "premultiplied" pixels (color already multiplied by the
/// alpha). The shell's icons usually are, but not always. A premultiplied
/// pixel never has a color channel brighter than its alpha, so if any pixel
/// does, the picture is "straight" and we premultiply it here.
pub fn premultiply_if_needed(bgra: &mut [u8]) {
    let (pixels, _) = bgra.as_chunks_mut::<4>();
    let straight = pixels.iter().any(|[b, g, r, a]| b > a || g > a || r > a);
    if !straight {
        return;
    }
    for p in pixels {
        let a = u16::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_pixels_are_left_alone() {
        // Half-transparent white, already premultiplied: (128,128,128,128).
        let mut px = vec![128, 128, 128, 128, 0, 0, 0, 0, 10, 20, 30, 255];
        let before = px.clone();
        premultiply_if_needed(&mut px);
        assert_eq!(px, before);
    }

    #[test]
    fn straight_pixels_get_premultiplied() {
        // Half-transparent white, straight alpha: (255,255,255,128).
        let mut px = vec![255, 255, 255, 128, 40, 80, 120, 255];
        premultiply_if_needed(&mut px);
        assert_eq!(px, vec![128, 128, 128, 128, 40, 80, 120, 255]);
    }
}
