//! Runs in its own process: the hardening applies to the whole test executable.
#![allow(unsafe_code)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken},
};

const PROCESS_VM_READ: u32 = 0x0010;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const WRITE_DAC: u32 = 0x0004_0000;

/// An elevated administrator token (CI runners use one) may hold SeDebugPrivilege, which bypasses
/// any process DACL. The hardening does not claim to stop administrators (SECURITY.md, T4).
fn elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut written = 0;
    // SAFETY: a query-only token of this process, closed below; the output buffer is a live
    // TOKEN_ELEVATION of the size passed.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut written,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[test]
fn after_hardening_the_process_cannot_be_opened_for_memory_reads() {
    let hardening = tg_platform::harden_process();
    assert_eq!(
        hardening,
        tg_platform::Hardening {
            dll_search: true,
            image_load: true,
            process_access: true,
        }
    );
    // Opening by PID goes through the access check like any other same-user program would.
    // SAFETY: plain handle requests by our own PID; any handle returned is closed.
    unsafe {
        let pid = GetCurrentProcessId();
        if elevated() {
            eprintln!("elevated token: the DACL is set, but administrators can bypass it");
        } else {
            for denied in [PROCESS_VM_READ, WRITE_DAC] {
                let handle = OpenProcess(denied, 0, pid);
                if !handle.is_null() {
                    CloseHandle(handle);
                }
                assert!(handle.is_null(), "access {denied:#x} must be denied");
            }
        }
        let allowed = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        assert!(
            !allowed.is_null(),
            "Task Manager style queries keep working"
        );
        CloseHandle(allowed);
    }
}
