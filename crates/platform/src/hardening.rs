//! Process hardening at startup, against malware running under the same Windows account while
//! the client is open (SECURITY.md, threat T3). Each measure is best effort: the client runs even
//! if Windows refuses one, and the result says which ones are in effect. This raises the cost of
//! an attack; it is not a security boundary against administrators or the kernel.
use std::ffi::c_void;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, Authorization::SE_KERNEL_OBJECT,
        Authorization::SetSecurityInfo, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
        GetLengthSid, GetTokenInformation, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION,
        SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser, WinCreatorOwnerRightsSid,
        WinLocalSystemSid,
    },
    System::{
        LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories},
        SystemServices::PROCESS_MITIGATION_IMAGE_LOAD_POLICY,
        Threading::{
            GetCurrentProcess, OpenProcessToken, ProcessImageLoadPolicy, SetProcessMitigationPolicy,
        },
    },
};

// Process access rights (winnt.h).
const PROCESS_TERMINATE: u32 = 0x0001;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const READ_CONTROL: u32 = 0x0002_0000;
const PROCESS_ALL_ACCESS: u32 = 0x001F_FFFF;
/// What other programs running as the same user may do: see the process, wait for it and end it
/// (Task Manager keeps working). Not: read or write its memory, create threads in it, duplicate
/// its handles or change its permissions.
const USER_RIGHTS: u32 = PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE | PROCESS_TERMINATE;

/// Which measures Windows accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hardening {
    /// Libraries loaded by name come only from System32, as in Telegram Desktop
    /// (`SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`): no DLL planting next to the
    /// executable or in the current directory. TDLib is loaded by absolute path.
    pub dll_search: bool,
    /// No images from network shares or from low-integrity (untrusted) locations.
    pub image_load: bool,
    /// The process DACL above, with the owner's implicit right to change it removed.
    pub process_access: bool,
}

/// Call once, first thing in `main`, before any window, engine or library is loaded.
pub fn harden_process() -> Hardening {
    // SAFETY: SetDefaultDllDirectories takes a flag value only.
    let dll_search = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } != 0;
    let mut policy = PROCESS_MITIGATION_IMAGE_LOAD_POLICY::default();
    // NoRemoteImages (bit 0) and NoLowMandatoryLabelImages (bit 1).
    policy.Anonymous.Flags = 0b11;
    // SAFETY: the pointer and size describe a live, initialized policy struct of the type the
    // policy class requires.
    let image_load = unsafe {
        SetProcessMitigationPolicy(
            ProcessImageLoadPolicy,
            (&raw const policy).cast::<c_void>(),
            size_of::<PROCESS_MITIGATION_IMAGE_LOAD_POLICY>(),
        )
    } != 0;
    Hardening {
        dll_search,
        image_load,
        process_access: restrict_process_access().is_some(),
    }
}

fn restrict_process_access() -> Option<()> {
    let user = current_user_sid()?;
    let system = well_known_sid(WinLocalSystemSid)?;
    // OWNER RIGHTS replaces the owner's implicit READ_CONTROL and WRITE_DAC, so a program running
    // as the same user cannot simply rewrite this DACL to grant itself memory access.
    let owner = well_known_sid(WinCreatorOwnerRightsSid)?;
    // Three ACCESS_ALLOWED_ACEs: header 8 bytes plus SID each; 512 bytes is ample.
    let mut acl = [0u64; 64];
    let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
    // SAFETY: `acl` is a writable, 8-byte aligned 512-byte buffer that outlives every call; each
    // SID buffer holds a valid SID written by Windows and lives until the end of this function.
    // GetCurrentProcess returns a pseudo-handle that needs no closing.
    unsafe {
        if InitializeAcl(acl_ptr, size_of_val(&acl) as u32, ACL_REVISION) == 0
            || AddAccessAllowedAce(acl_ptr, ACL_REVISION, USER_RIGHTS, user.as_ptr()) == 0
            || AddAccessAllowedAce(acl_ptr, ACL_REVISION, PROCESS_ALL_ACCESS, system.as_ptr()) == 0
            || AddAccessAllowedAce(acl_ptr, ACL_REVISION, READ_CONTROL, owner.as_ptr()) == 0
        {
            return None;
        }
        let status = SetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl_ptr,
            std::ptr::null(),
        );
        (status == 0).then_some(())
    }
}

/// A SID in an owned, aligned buffer.
struct Sid([u64; 9]);

impl Sid {
    /// For APIs that only read the SID (their PSID parameter is not const in the bindings).
    fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr().cast_mut().cast()
    }
    /// For APIs that write the SID.
    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

fn well_known_sid(kind: i32) -> Option<Sid> {
    let mut sid = Sid([0; 9]);
    let mut size = SECURITY_MAX_SID_SIZE;
    // SAFETY: the buffer is 72 bytes, at least SECURITY_MAX_SID_SIZE, and `size` says so.
    let ok = unsafe { CreateWellKnownSid(kind, std::ptr::null_mut(), sid.as_mut_ptr(), &mut size) };
    (ok != 0).then_some(sid)
}

fn current_user_sid() -> Option<Sid> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: a query-only token handle of this process, closed below on every path.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut buffer = [0u64; 64];
    let mut needed = 0;
    // SAFETY: the 512-byte aligned buffer receives a TOKEN_USER followed by its SID.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut needed,
        )
    };
    // SAFETY: closes the token handle opened above exactly once.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose Sid points inside the buffer;
    // the SID is copied out before the buffer goes away, and its length is checked.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let length = GetLengthSid(user.User.Sid) as usize;
        let mut sid = Sid([0; 9]);
        if length == 0 || length > size_of_val(&sid.0) {
            return None;
        }
        std::ptr::copy_nonoverlapping(
            user.User.Sid.cast::<u8>(),
            sid.0.as_mut_ptr().cast::<u8>(),
            length,
        );
        Some(sid)
    }
}
