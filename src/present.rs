//! Layered-window presenter. Tachyon's windows are borderless `WS_EX_LAYERED | WS_EX_TOOLWINDOW`
//! popups shown with `UpdateLayeredWindow(ULW_ALPHA)` from a premultiplied BGRA DIB. There is no
//! DWM backdrop, no frame extension and no window region, so alpha-0 pixels are truly transparent
//! and click-through, and size + position change atomically in one call.
//!
//! Reusable: the island/panel window uses it today; a future companion sprite window only needs
//! `create` + a `Surface` + `present`.
#![allow(unsafe_op_in_unsafe_fn)]
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn wz(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A 32-bpp top-down DIB section selected into a memory DC (premultiplied BGRA).
pub struct Surface {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub w: i32,
    pub h: i32,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Surface> {
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let dc = CreateCompatibleDC(null_mut());
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Surface { dc, bmp, old, bits: bits as *mut u8, w, h })
        }
    }
    /// Read-only pointer to the pixels (hit tests).
    pub fn bits_ptr(&self) -> *const u8 {
        self.bits
    }
    pub fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

/// Create a hidden layered popup. `extra_ex` adds e.g. `WS_EX_NOACTIVATE` / `WS_EX_TOPMOST`.
pub unsafe fn create(class: &str, title: &str, extra_ex: u32, hinst: HINSTANCE) -> HWND {
    let (c, t) = (wz(class), wz(title));
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | extra_ex,
        c.as_ptr(),
        t.as_ptr(),
        WS_POPUP | WS_SYSMENU,
        0,
        0,
        1,
        1,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if !hwnd.is_null() {
        no_dwm_chrome(hwnd);
    }
    hwnd
}

/// Windows 11: no DWM corner rounding and no DWM border around our own shape.
pub unsafe fn no_dwm_chrome(hwnd: HWND) {
    let pref: i32 = DWMWCP_DONOTROUND;
    DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &pref as *const _ as *const _, 4);
    let none: u32 = DWMWA_COLOR_NONE;
    DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR as u32, &none as *const _ as *const _, 4);
}

/// Show the surface at screen position (x, y) (top-left of the padded image).
pub unsafe fn present(hwnd: HWND, s: &Surface, x: i32, y: i32) -> bool {
    let pos = POINT { x, y };
    let size = SIZE { cx: s.w, cy: s.h };
    let src = POINT { x: 0, y: 0 };
    let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
    UpdateLayeredWindow(hwnd, null_mut(), &pos, &size, s.dc, &src, 0, &bf, ULW_ALPHA) != 0
}

pub unsafe fn set_ex_flag(hwnd: HWND, flag: u32, on: bool) {
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let new = if on { ex | flag } else { ex & !flag };
    if new != ex {
        SetWindowLongW(hwnd, GWL_EXSTYLE, new as i32);
    }
}

pub unsafe fn set_topmost(hwnd: HWND, on: bool) {
    SetWindowPos(hwnd, if on { HWND_TOPMOST } else { HWND_NOTOPMOST }, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
}

/// Experimental `glass = "blur"`: ask DWM to blur what is behind the window (undocumented
/// `SetWindowCompositionAttribute`, ACCENT_ENABLE_BLURBEHIND, which does not depend on focus),
/// clipped to the glass shape with a window region. `shape` = (x, y, w, h, radius) in window px;
/// None turns it off. Returns false when the API is missing.
pub unsafe fn set_blur(hwnd: HWND, shape: Option<(i32, i32, i32, i32, i32)>) -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    #[repr(C)]
    struct Accent {
        state: u32,
        flags: u32,
        color: u32,
        anim: u32,
    }
    #[repr(C)]
    struct Data {
        attrib: u32,
        data: *mut core::ffi::c_void,
        size: usize,
    }
    let user32 = GetModuleHandleW(wz("user32.dll").as_ptr());
    let Some(f) = GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr() as *const u8) else { return false };
    let f: unsafe extern "system" fn(HWND, *mut Data) -> i32 = std::mem::transmute(f);
    let mut acc = Accent { state: if shape.is_some() { 3 } else { 0 }, flags: 0, color: 0, anim: 0 };
    let mut d = Data { attrib: 19, data: &mut acc as *mut _ as *mut _, size: size_of::<Accent>() };
    let ok = f(hwnd, &mut d) != 0;
    match shape {
        Some((x, y, w, h, r)) => {
            let rgn = CreateRoundRectRgn(x, y, x + w + 1, y + h + 1, 2 * r, 2 * r);
            SetWindowRgn(hwnd, rgn, 1);
        }
        None => {
            SetWindowRgn(hwnd, null_mut(), 1);
        }
    }
    ok
}

/// Windows "Transparency effects" (Settings > Personalisation > Colours): Some(true/false), or
/// None when the value is missing (Windows 7/8, Wine).
pub fn transparency_effects() -> Option<bool> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    unsafe {
        let key = wz(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let val = wz("EnableTransparency");
        let mut v: u32 = 0;
        let mut sz: u32 = 4;
        let r =
            RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, null_mut(), &mut v as *mut _ as *mut _, &mut sz);
        if r == 0 { Some(v != 0) } else { None }
    }
}
