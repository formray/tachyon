//! Kernel objects that only the current user can open: the agent snapshot and the
//! single-instance mutex get an explicit owner and DACL (this user and SYSTEM) and a
//! medium-integrity no-read-up label, so sandboxed low-integrity processes cannot read them.
//! Readers check the owner before trusting what they open.
#![cfg(windows)]

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The current user's SID (TOKEN_USER buffer; the SID points into it).
struct UserSid(Vec<u64>);

impl UserSid {
    fn get() -> Option<UserSid> {
        unsafe {
            let mut tok: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return None;
            }
            let mut need = 0u32;
            GetTokenInformation(tok, TokenUser, std::ptr::null_mut(), 0, &mut need);
            let mut buf = vec![0u64; (need as usize).div_ceil(8).max(8)];
            let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr() as *mut c_void, (buf.len() * 8) as u32, &mut need);
            CloseHandle(tok);
            (ok != 0).then_some(UserSid(buf))
        }
    }
    fn psid(&self) -> PSID {
        unsafe { (*(self.0.as_ptr() as *const TOKEN_USER)).User.Sid }
    }
    fn string(&self) -> Option<String> {
        unsafe {
            let mut p: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(self.psid(), &mut p) == 0 || p.is_null() {
                return None;
            }
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            LocalFree(p as *mut c_void);
            Some(s)
        }
    }
}

/// The SDDL used for our objects (pure, for the tests).
pub fn sddl(user_sid: &str, label: bool) -> String {
    let mut s = format!("O:{user_sid}D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)");
    if label {
        s.push_str("S:(ML;;NWNR;;;ME)");
    }
    s
}

/// Security attributes for an object only this user can open. Keep it alive while creating.
pub struct Private {
    sd: PSECURITY_DESCRIPTOR,
    pub sa: SECURITY_ATTRIBUTES,
}

impl Private {
    pub fn new() -> Option<Private> {
        let sid = UserSid::get()?.string()?;
        // wine and old systems may refuse the integrity label: then the DACL alone
        for label in [true, false] {
            let w: Vec<u16> = sddl(&sid, label).encode_utf16().chain([0]).collect();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let ok =
                unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) };
            if ok != 0 && !sd.is_null() {
                let sa =
                    SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
                return Some(Private { sd, sa });
            }
        }
        None
    }
    pub fn ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.sa
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.sd);
        }
    }
}

/// True when the kernel object behind `h` is owned by the current user.
// `h` is only handed to the OS (an invalid handle makes GetSecurityInfo fail), never dereferenced.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn owned_by_me(h: HANDLE) -> bool {
    let Some(me) = UserSid::get() else { return false };
    unsafe {
        let mut owner: PSID = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let r = GetSecurityInfo(
            h,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        );
        let mine = r == 0 && !owner.is_null() && EqualSid(owner, me.psid()) != 0;
        if !sd.is_null() {
            LocalFree(sd);
        }
        mine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Memory::{CreateFileMappingW, PAGE_READWRITE};

    #[test]
    fn only_this_user_and_system() {
        let s = sddl("S-1-5-21-1-2-3-1001", true);
        assert_eq!(s, "O:S-1-5-21-1-2-3-1001D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)S:(ML;;NWNR;;;ME)");
        assert!(!s.contains(";WD)") && !s.contains(";AU)") && !s.contains(";BU)"), "no Everyone, Authenticated Users or Users");
    }

    #[test]
    fn a_private_mapping_is_ours() {
        let p = Private::new().expect("security descriptor");
        let name: Vec<u16> = format!("Local\\TachyonTest{}", std::process::id()).encode_utf16().chain([0]).collect();
        unsafe {
            let h = CreateFileMappingW(INVALID_HANDLE_VALUE, p.ptr(), PAGE_READWRITE, 0, 4096, name.as_ptr());
            assert!(!h.is_null());
            assert!(owned_by_me(h));
            CloseHandle(h);
        }
    }
}
