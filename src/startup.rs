//! "Start with Windows": a shortcut to tachyon.exe in the user's Startup folder
//! (%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup). No registry, no admin rights;
//! the shortcut itself is the setting, so deleting it by hand turns it off too.
#![cfg(windows)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows_sys::core::GUID;

const LINK_NAME: &str = "Tachyon.lnk";
const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
const IID_ISHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
const IID_IPERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

/// The per-user Startup folder (None without %APPDATA%).
pub fn folder() -> Option<PathBuf> {
    let a = std::env::var_os("APPDATA").filter(|a| !a.is_empty())?;
    Some(PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
}

pub fn link_path() -> Option<PathBuf> {
    folder().map(|f| f.join(LINK_NAME))
}

pub fn enabled() -> bool {
    link_path().is_some_and(|p| p.exists())
}

/// Turn it on (a shortcut to `exe`) or off (remove our shortcut).
pub fn set(on: bool, exe: &Path) -> Result<(), String> {
    let link = link_path().ok_or("No Startup folder (APPDATA is not set)")?;
    if on {
        if let Some(d) = link.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        write_link(&link, exe, "Tachyon GPU monitor")
    } else {
        match std::fs::remove_file(&link) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn wz(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

type Raw = *mut c_void;
/// The n-th method of a COM object's vtable.
unsafe fn method(obj: Raw, n: usize) -> *const c_void {
    unsafe { *(*(obj as *const *const *const c_void)).add(n) }
}

/// Write a .lnk at `link` pointing to `target` (IShellLinkW + IPersistFile).
pub fn write_link(link: &Path, target: &Path, description: &str) -> Result<(), String> {
    type SetStr = unsafe extern "system" fn(Raw, *const u16) -> i32;
    type Qi = unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32;
    type Release = unsafe extern "system" fn(Raw) -> u32;
    type Save = unsafe extern "system" fn(Raw, *const u16, i32) -> i32;
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut sl: Raw = std::ptr::null_mut();
        let hr = CoCreateInstance(&CLSID_SHELL_LINK, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &IID_ISHELL_LINK_W, &mut sl);
        if hr < 0 || sl.is_null() {
            return Err(format!("could not create a shortcut (0x{hr:08x})"));
        }
        let call = |n: usize, s: &str| {
            let f: SetStr = std::mem::transmute(method(sl, n));
            f(sl, wz(s).as_ptr())
        };
        let dir = target.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let mut hr = call(20, &target.to_string_lossy()); // SetPath
        if hr >= 0 {
            hr = call(9, &dir); // SetWorkingDirectory
        }
        if hr >= 0 {
            hr = call(7, description); // SetDescription
        }
        let mut pf: Raw = std::ptr::null_mut();
        if hr >= 0 {
            let qi: Qi = std::mem::transmute(method(sl, 0));
            hr = qi(sl, &IID_IPERSIST_FILE, &mut pf);
        }
        if hr >= 0 && !pf.is_null() {
            let save: Save = std::mem::transmute(method(pf, 6)); // IPersistFile::Save
            hr = save(pf, wz(&link.to_string_lossy()).as_ptr(), 1);
            let rel: Release = std::mem::transmute(method(pf, 2));
            rel(pf);
        }
        let rel: Release = std::mem::transmute(method(sl, 2));
        rel(sl);
        if hr < 0 { Err(format!("could not save the shortcut (0x{hr:08x})")) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_shortcut_file() {
        let d = std::env::temp_dir().join(format!("tachyon_lnk_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let link = d.join("Tachyon.lnk");
        let exe = std::env::current_exe().unwrap();
        write_link(&link, &exe, "test").unwrap();
        let b = std::fs::read(&link).unwrap();
        assert!(b.len() > 76 && b[..4] == [0x4c, 0, 0, 0], "a Shell Link file");
        let _ = std::fs::remove_dir_all(&d);
    }
}
